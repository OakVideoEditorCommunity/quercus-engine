//! Parameter introspection and editing on live instances
//! (`quercus_ofx_param_*` / `quercus_ofx_instance_param_count`).
//!
//! Values are read/written with OFX paramSetValue semantics (the
//! instance's current value; no undo, no node bridge — headless
//! consumers drive instances directly). Setters keep the parameter's
//! declared arity: a `dim` that mismatches the parameter's dimension
//! fails.

use quercus_ofx_plugin::node_factory;
use quercus_ofx_plugin::param::{ParamInstance, ParamValue};

use crate::codec::{copy_str_out, str_arg};

/// `kOfxPropLabel` on parameter definitions (ofxCore.h: plugins set
/// the generic label property on param props too).
const PROP_LABEL: &str = "OfxPropLabel";

/// Runs `f` on the instance's parameter `name`; None when the id or
/// the parameter is unknown.
fn with_param<R>(id: u64, name: &str, f: impl FnOnce(&ParamInstance) -> R) -> Option<R> {
	let inst = node_factory::instance_from_id(id)?;
	let p = inst.value.params.find(name)?;
	Some(f(p))
}

/// Runs `f` on the instance's parameter at `index` (definition
/// order); None when the id or the index is unknown.
fn with_param_at<R>(id: u64, index: i32, f: impl FnOnce(&ParamInstance) -> R) -> Option<R> {
	if index < 0 {
		return None;
	}
	let inst = node_factory::instance_from_id(id)?;
	let p = inst.value.params.params.get(index as usize)?;
	Some(f(p))
}

/// Number of parameters on the instance; -1 when the id is unknown.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_instance_param_count(id: u64) -> i32 {
	node_factory::instance_from_id(id)
		.map(|inst| inst.value.params.params.len() as i32)
		.unwrap_or(-1)
}

/// Reads a parameter's name by definition index, snprintf-style.
///
/// Names are the keys accepted by the typed getters/setters in this module
/// and by [`quercus_ofx_instance_param_count`] to enumerate the definition
/// order. The text is NUL-terminated in `buf` and the full byte length
/// excluding the NUL is returned, so a null/zero-length buffer queries the
/// required size.
///
/// # Returns
///
/// `-1` when `id` is unknown or `index` is negative/out of range.
///
/// # Safety
///
/// `buf` must be null or point to at least `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_name_at(
	id: u64,
	index: i32,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	match with_param_at(id, index, |p| p.def.name.clone()) {
		Some(name) => copy_str_out(&name, buf, buf_len),
		None => -1,
	}
}

/// Reads a parameter's OFX type string (`kOfxParamType*`) by definition
/// index, snprintf-style.
///
/// This is the declared type of the parameter at `index` (definition order,
/// including pages/groups), useful for choosing the matching typed getter.
/// The text is NUL-terminated in `buf` and the full byte length excluding the
/// NUL is returned, so a null/zero-length buffer queries the required size.
///
/// # Returns
///
/// `-1` when `id` is unknown or `index` is negative/out of range.
///
/// # Safety
///
/// `buf` must be null or point to at least `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_type_at(
	id: u64,
	index: i32,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	match with_param_at(id, index, |p| p.def.ofx_type.clone()) {
		Some(ty) => copy_str_out(&ty, buf, buf_len),
		None => -1,
	}
}

/// Reads a parameter's display label (`kOfxPropLabel`) by definition index,
/// snprintf-style.
///
/// The label is the OFX generic label property; when the definition does not
/// carry one, an empty string is written and `0` is returned (not `-1`), so a
/// zero-length result can mean "no label". The text is NUL-terminated in
/// `buf` and the full byte length excluding the NUL is returned, so a
/// null/zero-length buffer queries the required size.
///
/// # Returns
///
/// `-1` when `id` is unknown or `index` is negative/out of range.
///
/// # Safety
///
/// `buf` must be null or point to at least `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_label_at(
	id: u64,
	index: i32,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let label = with_param_at(id, index, |p| match p.def.props.get(PROP_LABEL, 0) {
		Some(quercus_ofx_plugin::property::Value::String(s)) => s.to_string_lossy().into_owned(),
		_ => String::new(),
	});
	match label {
		Some(label) => copy_str_out(&label, buf, buf_len),
		None => -1,
	}
}

// ---------------------------------------------------------------------------
// Typed get/set (by parameter name)
// ---------------------------------------------------------------------------

/// Reads an integer-family parameter (Integer/Integer2D/Integer3D) into
/// `out`.
///
/// Writes `min(dim, cap)` components into `out`; a null `out` only queries
/// the dimension.
///
/// # Returns
///
/// The parameter's dimension (1..=3), or `-1` on an unknown `id`/`name` or a
/// parameter of another type.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8,
/// and `out` must be null or point to at least `cap` writable `i32` values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_get_int(
	id: u64,
	name: *const u8,
	name_len: usize,
	out: *mut i32,
	cap: usize,
) -> i32 {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return -1;
	};
	let Some(dim) = with_param(id, name, |p| match p.get() {
		ParamValue::Int(a, dim) => {
			if !out.is_null() {
				for i in 0..dim.min(cap) {
					unsafe { *out.add(i) = a[i] };
				}
			}
			dim as i32
		}
		_ => -1,
	}) else {
		return -1;
	};
	dim
}

/// Writes an integer-family parameter from `values`, keeping the declared
/// arity.
///
/// `dim` is the number of components supplied and must equal the parameter's
/// arity (Integer = 1, Integer2D = 2, Integer3D = 3); it must also lie in
/// `1..=3`. Values are set with OFX `paramSetValue` semantics (current value
/// only, no undo).
///
/// # Returns
///
/// `false` on an unknown `id` or `name`, a parameter of another type, a `dim`
/// outside `1..=3`, a `dim` that disagrees with the parameter's arity, or a
/// null `values`.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8,
/// and `values` must be null or point to at least `dim` readable `i32`
/// values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_set_int(
	id: u64,
	name: *const u8,
	name_len: usize,
	values: *const i32,
	dim: usize,
) -> bool {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return false;
	};
	if values.is_null() || !(1..=3).contains(&dim) {
		return false;
	}
	with_param(id, name, |p| {
		let current = p.get();
		if let ParamValue::Int(_, d) = current {
			if d != dim {
				return false;
			}
			let mut a = [0i32; 3];
			for i in 0..dim {
				a[i] = unsafe { *values.add(i) };
			}
			p.set_ofx(ParamValue::Int(a, dim));
			return true;
		}
		false
	})
	.unwrap_or(false)
}

/// Reads a double-family parameter (Double/Double2D/Double3D) into `out`.
///
/// Writes `min(dim, cap)` components into `out`; a null `out` only queries
/// the dimension.
///
/// # Returns
///
/// The parameter's dimension (1..=3), or `-1` on an unknown `id`/`name` or a
/// parameter of another type.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8,
/// and `out` must be null or point to at least `cap` writable `f64` values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_get_double(
	id: u64,
	name: *const u8,
	name_len: usize,
	out: *mut f64,
	cap: usize,
) -> i32 {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return -1;
	};
	let Some(dim) = with_param(id, name, |p| match p.get() {
		ParamValue::Double(a, dim) => {
			if !out.is_null() {
				for i in 0..dim.min(cap) {
					unsafe { *out.add(i) = a[i] };
				}
			}
			dim as i32
		}
		_ => -1,
	}) else {
		return -1;
	};
	dim
}

/// Writes a double-family parameter from `values`, keeping the declared
/// arity.
///
/// `dim` is the number of components supplied and must equal the parameter's
/// arity (Double = 1, Double2D = 2, Double3D = 3); it must also lie in
/// `1..=3`. Values are set with OFX `paramSetValue` semantics (current value
/// only, no undo).
///
/// # Returns
///
/// `false` on an unknown `id` or `name`, a parameter of another type, a `dim`
/// outside `1..=3`, a `dim` that disagrees with the parameter's arity, or a
/// null `values`.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8,
/// and `values` must be null or point to at least `dim` readable `f64`
/// values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_set_double(
	id: u64,
	name: *const u8,
	name_len: usize,
	values: *const f64,
	dim: usize,
) -> bool {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return false;
	};
	if values.is_null() || !(1..=3).contains(&dim) {
		return false;
	}
	with_param(id, name, |p| {
		let current = p.get();
		if let ParamValue::Double(_, d) = current {
			if d != dim {
				return false;
			}
			let mut a = [0.0f64; 3];
			for i in 0..dim {
				a[i] = unsafe { *values.add(i) };
			}
			p.set_ofx(ParamValue::Double(a, dim));
			return true;
		}
		false
	})
	.unwrap_or(false)
}

/// Reads a boolean parameter.
///
/// # Returns
///
/// `1` for true and `0` for false, or `-1` on an unknown `id`/`name` or a
/// parameter of another type (so `-1` must be checked before using the value
/// as a boolean).
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8
/// for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_get_bool(id: u64, name: *const u8, name_len: usize) -> i32 {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return -1;
	};
	with_param(id, name, |p| match p.get() {
		ParamValue::Bool(b) => b as i32,
		_ => -1,
	})
	.unwrap_or(-1)
}

/// Writes a boolean parameter.
///
/// Values are set with OFX `paramSetValue` semantics (current value only, no
/// undo); writing the value that is already stored still succeeds.
///
/// # Returns
///
/// `false` on an unknown `id` or `name`, or a parameter of another type.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8
/// for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_set_bool(
	id: u64,
	name: *const u8,
	name_len: usize,
	value: bool,
) -> bool {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return false;
	};
	with_param(id, name, |p| {
		if matches!(p.get(), ParamValue::Bool(_)) {
			p.set_ofx(ParamValue::Bool(value));
			return true;
		}
		false
	})
	.unwrap_or(false)
}

/// Reads a choice parameter (the 0-based option index).
///
/// # Returns
///
/// The current option index, or `-1` on an unknown `id`/`name` or a parameter
/// of another type (the return value cannot distinguish the two).
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8
/// for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_get_choice(
	id: u64,
	name: *const u8,
	name_len: usize,
) -> i32 {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return -1;
	};
	with_param(id, name, |p| match p.get() {
		ParamValue::Choice(c) => c,
		_ => -1,
	})
	.unwrap_or(-1)
}

/// Writes a choice parameter (the 0-based option index).
///
/// The index is stored as given: it is not validated against the option list,
/// so callers are responsible for staying in range. Values are set with OFX
/// `paramSetValue` semantics (current value only, no undo).
///
/// # Returns
///
/// `false` on an unknown `id` or `name`, or a parameter of another type.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8
/// for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_set_choice(
	id: u64,
	name: *const u8,
	name_len: usize,
	index: i32,
) -> bool {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return false;
	};
	with_param(id, name, |p| {
		if matches!(p.get(), ParamValue::Choice(_)) {
			p.set_ofx(ParamValue::Choice(index));
			return true;
		}
		false
	})
	.unwrap_or(false)
}

/// Reads a color parameter (RGB/RGBA) into `out`.
///
/// Writes `min(dim, cap)` components into `out`; a null `out` only queries
/// the dimension. The values are the parameter's color components in the
/// order the parameter declares them.
///
/// # Returns
///
/// The parameter's dimension (3 or 4), or `-1` on an unknown `id`/`name` or a
/// parameter of another type.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8,
/// and `out` must be null or point to at least `cap` writable `f64` values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_get_color(
	id: u64,
	name: *const u8,
	name_len: usize,
	out: *mut f64,
	cap: usize,
) -> i32 {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return -1;
	};
	let Some(dim) = with_param(id, name, |p| match p.get() {
		ParamValue::Color(a, dim) => {
			if !out.is_null() {
				for i in 0..dim.min(cap) {
					unsafe { *out.add(i) = a[i] };
				}
			}
			dim as i32
		}
		_ => -1,
	}) else {
		return -1;
	};
	dim
}

/// Writes a color parameter from `values`.
///
/// `dim` is the number of components supplied (`3` = RGB, `4` = RGBA) and
/// must equal the parameter's declared arity. Values are set with OFX
/// `paramSetValue` semantics (current value only, no undo).
///
/// # Returns
///
/// `false` on an unknown `id` or `name`, a non-color parameter, a `dim`
/// outside `3..=4`, a `dim` that disagrees with the parameter's arity, or a
/// null `values`.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8,
/// and `values` must be null or point to at least `dim` readable `f64`
/// values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_set_color(
	id: u64,
	name: *const u8,
	name_len: usize,
	values: *const f64,
	dim: usize,
) -> bool {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return false;
	};
	if values.is_null() || !(3..=4).contains(&dim) {
		return false;
	}
	with_param(id, name, |p| {
		let current = p.get();
		if let ParamValue::Color(_, d) = current {
			if d != dim {
				return false;
			}
			let mut a = [0.0f64; 4];
			for i in 0..dim {
				a[i] = unsafe { *values.add(i) };
			}
			p.set_ofx(ParamValue::Color(a, dim));
			return true;
		}
		false
	})
	.unwrap_or(false)
}

/// Reads a string-family parameter (String/StrChoice), snprintf-style.
///
/// The text is NUL-terminated in `buf` and the full byte length (excluding
/// the NUL) is returned, so a null/zero-length buffer queries the required
/// size and `ret >= buf_len` means the output was truncated. Invalid UTF-8 in
/// the stored value is replaced with U+FFFD rather than failing.
///
/// # Returns
///
/// `-1` on an unknown `id` or `name`, or a parameter of another type.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8,
/// and `buf` must be null or point to at least `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_get_string(
	id: u64,
	name: *const u8,
	name_len: usize,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return -1;
	};
	let value = with_param(id, name, |p| match p.get() {
		ParamValue::String(s) | ParamValue::StrChoice(s) => Some(s.to_string_lossy().into_owned()),
		_ => None,
	});
	match value.flatten() {
		Some(s) => copy_str_out(&s, buf, buf_len),
		None => -1,
	}
}

/// Writes a string-family parameter (String/StrChoice).
///
/// The parameter keeps its family: a `String` parameter receives a `String`
/// value and a `StrChoice` parameter a `StrChoice` value. Values are set with
/// OFX `paramSetValue` semantics (current value only, no undo).
///
/// # Returns
///
/// `false` on an unknown `id` or `name`, a parameter of another type, a
/// null/non-UTF-8 `value`, or a `value` containing an interior NUL byte.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8,
/// and `value` must be null or point to `value_len` readable bytes of valid
/// UTF-8, for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_set_string(
	id: u64,
	name: *const u8,
	name_len: usize,
	value: *const u8,
	value_len: usize,
) -> bool {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return false;
	};
	let Some(value) = (unsafe { str_arg(value, value_len) }) else {
		return false;
	};
	let Ok(value) = std::ffi::CString::new(value) else {
		return false;
	};
	with_param(id, name, |p| {
		match p.get() {
			ParamValue::String(_) => p.set_ofx(ParamValue::String(value)),
			ParamValue::StrChoice(_) => p.set_ofx(ParamValue::StrChoice(value)),
			_ => return false,
		}
		true
	})
	.unwrap_or(false)
}

/// Reads a parametric parameter as the curve-set JSON (the same payload node
/// values carry), snprintf-style.
///
/// The JSON text is NUL-terminated in `buf` and the full byte length
/// (excluding the NUL) is returned, so a null/zero-length buffer queries the
/// required size and `ret >= buf_len` means the output was truncated. The
/// counterpart setter is [`quercus_ofx_param_set_curves_json`].
///
/// # Returns
///
/// `-1` on an unknown `id` or `name`, or a parameter that is not parametric.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8,
/// and `buf` must be null or point to at least `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_get_curves_json(
	id: u64,
	name: *const u8,
	name_len: usize,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return -1;
	};
	let json = with_param(id, name, |p| match p.get() {
		ParamValue::Parametric(curves) => {
			Some(quercus_ofx_plugin::param_curve::curves_to_json(&curves))
		}
		_ => None,
	});
	match json.flatten() {
		Some(json) => copy_str_out(&json, buf, buf_len),
		None => -1,
	}
}

/// Writes a parametric parameter from curve-set JSON (see
/// [`crate::ofx::curve`]).
///
/// The JSON must be the same curve-set payload returned by
/// [`quercus_ofx_param_get_curves_json`]; it is decoded before the instance is
/// touched, so malformed input never mutates the parameter. Values are set
/// with OFX `paramSetValue` semantics (current value only, no undo).
///
/// # Returns
///
/// `false` on an unknown `id` or `name`, a parameter that is not parametric,
/// a null/non-UTF-8 `json`, or JSON that does not decode as a curve set.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8,
/// and `json` must be null or point to `json_len` readable bytes of valid
/// UTF-8, for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_param_set_curves_json(
	id: u64,
	name: *const u8,
	name_len: usize,
	json: *const u8,
	json_len: usize,
) -> bool {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return false;
	};
	let Some(json) = (unsafe { str_arg(json, json_len) }) else {
		return false;
	};
	let Some(curves) = quercus_ofx_plugin::param_curve::curves_from_json(json) else {
		return false;
	};
	with_param(id, name, |p| {
		if matches!(p.get(), ParamValue::Parametric(_)) {
			p.set_ofx(ParamValue::Parametric(curves));
			return true;
		}
		false
	})
	.unwrap_or(false)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn unknown_instance_and_param_fail_closed() {
		assert_eq!(quercus_ofx_instance_param_count(u64::MAX), -1);
		let mut buf = [0u8; 16];
		assert_eq!(
			unsafe { quercus_ofx_param_name_at(u64::MAX, 0, buf.as_mut_ptr(), 16) },
			-1
		);
		let mut d = [0.0f64; 3];
		assert_eq!(
			unsafe { quercus_ofx_param_get_double(u64::MAX, "gain".as_ptr(), 4, d.as_mut_ptr(), 3) },
			-1
		);
		assert!(!unsafe { quercus_ofx_param_set_bool(u64::MAX, "b".as_ptr(), 1, true) });
		assert_eq!(
			unsafe { quercus_ofx_param_get_string(u64::MAX, "s".as_ptr(), 1, buf.as_mut_ptr(), 16) },
			-1
		);
		assert!(!unsafe {
			quercus_ofx_param_set_curves_json(u64::MAX, "c".as_ptr(), 1, "[]".as_ptr(), 2)
		});
		// Malformed JSON never reaches the instance.
		assert!(!unsafe {
			quercus_ofx_param_set_curves_json(u64::MAX, "c".as_ptr(), 1, "{".as_ptr(), 1)
		});
	}
}
