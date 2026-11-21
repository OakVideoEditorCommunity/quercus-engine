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

//! OfxPropertySuite v1: property reads and writes.
//!
//! Semantics follow HS: ofxhPropertySuite.cpp case by case:
//! - Reading an undefined property / a type mismatch → kOfxStatErrUnknown (HS:
//!   propGet's `fetchTypedProperty` failure path,
//!   ofxhPropertySuite.cpp:787-794);
//! - Out-of-bounds reads/writes → kOfxStatErrBadIndex (HS: `getValueRaw`/
//!   `setValue`, ofxhPropertySuite.cpp:257/284);
//! - propGetN copies min(count, dimension) elements and does not report an
//!   error (HS: `getValueNRaw`, ofxhPropertySuite.cpp:271-280);
//! - propSetN replaces the property wholesale when the dimension changes: this
//!   crate's property dimension always follows the array length (for a
//!   fixed-dimension property HS returns kOfxStatErrBadIndex when
//!   count > dimension, ofxhPropertySuite.cpp:299 — this crate does not
//!   distinguish fixed/variable dimensions and is faithfully more lenient; all
//!   three system bundles call with the correct count, so there is no behavioral
//!   divergence);
//! - Read-only properties: the SDK has no kOfxStatErrReadOnly, and HostSupport
//!   does not intercept writes at the suite layer either (`_pluginReadOnly` is
//!   only queried by host-internal logic) — this implementation faithfully does
//!   not intercept; if that is needed later, add a read-only flag to PropertySet
//!   and re-evaluate.
//! - propReset: HostSupport restores the define-time default values
//!   (ofxhPropertySuite.cpp:330); this crate does not keep a snapshot of the
//!   defaults, so phase 1 deliberately returns kOfxStatErrUnsupported (telling
//!   the truth is safer than a silent no-op).

use std::env;
use std::ffi::{c_char, c_double, c_int, c_void, CStr, CString};
use std::panic;
use std::sync;

use crate::property::{Property, PropertySet, Value};
use crate::suites::status;

/// Function table layout (field-for-field identical to the SDK's
/// `OfxPropertySuiteV1`).
#[repr(C)]
pub struct PropertySuiteV1 {
	/// propSetPointer
	pub set_pointer: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int,
	/// propSetString
	pub set_string: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const c_char) -> c_int,
	/// propSetDouble
	pub set_double: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, c_double) -> c_int,
	/// propSetInt
	pub set_int: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, c_int) -> c_int,
	/// propSetPointerN
	pub set_pointer_n:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const *mut c_void) -> c_int,
	/// propSetStringN
	pub set_string_n:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const *const c_char) -> c_int,
	/// propSetDoubleN
	pub set_double_n:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const c_double) -> c_int,
	/// propSetIntN
	pub set_int_n: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const c_int) -> c_int,
	/// propGetPointer
	pub get_pointer:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut *mut c_void) -> c_int,
	/// propGetString
	pub get_string:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut *mut c_char) -> c_int,
	/// propGetDouble
	pub get_double: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_double) -> c_int,
	/// propGetInt
	pub get_int: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_int) -> c_int,
	/// propGetPointerN
	pub get_pointer_n:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut *mut c_void) -> c_int,
	/// propGetStringN
	pub get_string_n:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut *mut c_char) -> c_int,
	/// propGetDoubleN
	pub get_double_n:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_double) -> c_int,
	/// propGetIntN
	pub get_int_n: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_int) -> c_int,
	/// propReset
	pub reset: unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int,
	/// propGetDimension
	pub get_dimension: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int) -> c_int,
}

/// Property value kind (the suite-layer type check; corresponds to HS's
/// `Property::TypeEnum`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
	Int,
	Double,
	Str,
	Pointer,
}

impl Kind {
	fn of(v: &Value) -> Kind {
		match v {
			Value::Int(_) => Kind::Int,
			Value::Double(_) => Kind::Double,
			Value::String(_) => Kind::Str,
			Value::Pointer(_) => Kind::Pointer,
		}
	}
}

/// Common entry template for the property suite: decode the handle + panic
/// guard.
///
/// Handles follow the [`crate::suites::tag`] convention: the low 3 bits are the
/// object-kind tag and the address is the object's props field (offset 0); once
/// the tag is stripped it can be used directly as a PropertySet. A bare
/// PropertySet pointer (tag 0, passed directly by host internals/tests) passes
/// through unchanged.
///
/// `# Safety`: `handle` must point to a live `PropertySet` or a registered
/// object (the suite lifetime contract: host objects are created before suite
/// calls and destroyed after all of them).
#[track_caller]
unsafe fn caught(handle: *mut c_void, f: impl FnOnce(&PropertySet) -> Result<(), c_int>) -> c_int {
	let caller = panic::Location::caller();
	let code = panic::catch_unwind(panic::AssertUnwindSafe(|| {
		if handle.is_null() {
			return status::ERR_BAD_HANDLE;
		}
		let set = unsafe { &*crate::suites::tag::strip(handle) };
		f(set).map_or_else(|code| code, |()| status::OK)
	}))
	.unwrap_or(status::FAILED);
	if code != status::OK && env::var_os("QUERCUS_OFX_TRACE").is_some() {
		let tag = crate::suites::tag::kind(handle);
		eprintln!("[ofx] property suite error {code} at {caller} (handle {handle:p} tag {tag})");
	}
	code
}

/// Property name: null pointer / non-UTF-8 → kOfxStatErrValue (defensive;
/// HostSupport dereferences directly and would crash).
///
/// `# Safety`: `name` must be a valid NUL-terminated C string (plugin contract).
unsafe fn c_name<'a>(name: *const c_char) -> Result<&'a str, c_int> {
	if name.is_null() {
		return Err(status::ERR_VALUE);
	}
	unsafe { CStr::from_ptr(name) }
		.to_str()
		.map_err(|_| status::ERR_VALUE)
}

/// Index guard: converts a `c_int` index received at the suite layer into
/// `usize`.
///
/// A negative `index` cannot be represented as `usize`, so it is treated as out
/// of bounds directly → kOfxStatErrBadIndex (it must not silently wrap into a
/// huge index). Every propGet/propSet entry point goes through this before
/// touching the array.
fn idx(index: c_int) -> Result<usize, c_int> {
	usize::try_from(index).map_err(|_| status::ERR_BAD_INDEX)
}

/// Value lookup with the type check before the index check (HS order:
/// `fetchTypedProperty` before `getValueRaw`, ofxhPropertySuite.cpp:787/794/257).
fn get_value<'a>(
	props: &'a [Property],
	name: &str,
	index: c_int,
	kind: Kind,
) -> Result<&'a Value, c_int> {
	let result = get_value_inner(props, name, index, kind);
	if env::var_os("QUERCUS_OFX_TRACE").is_some() {
		match &result {
			Ok(_) => eprintln!("[ofx] propGet({name}[{index}]) -> ok"),
			Err(c) => eprintln!("[ofx] propGet({name}[{index}]) -> {c}"),
		}
	}
	result
}

/// Inner implementation of single-element reads: first find the property named
/// `name` in `props`, check its type, then take the value at `index`.
///
/// The type is represented by the first element for the whole property (the host
/// only defines homogeneous arrays; HS fixes the type at define time, which is
/// equivalent). An empty array has no type probe, so the type check is skipped
/// and out of bounds is reported as BadIndex by the indexed access. Int and
/// Double are treated as the same numeric kind and may be read from each other.
/// `props` is the property list to search, `name` is the property name, and
/// `index` is the element index (`c_int`; negative values fail to convert to
/// usize here).
///
/// # Errors
///
/// Undefined property → kOfxStatErrUnknown; first-element type does not match
/// `kind` and is not a numeric read-across → kOfxStatErrUnknown; negative index
/// or out of bounds → kOfxStatErrBadIndex.
fn get_value_inner<'a>(
	props: &'a [Property],
	name: &str,
	index: c_int,
	kind: Kind,
) -> Result<&'a Value, c_int> {
	let p = props
		.iter()
		.find(|p| p.name == name)
		.ok_or(status::ERR_UNKNOWN)?;
	// The first element proxies the type of the whole property (the host only defines
	// homogeneous arrays; HS fixes the type at define time, equivalent). An empty array has no
	// type probe, so skip the type check and report BadIndex from the index access below
	// (HS getValueRaw semantics).
	if let Some(probe) = p.values.first() {
		// Int <-> Double numeric coercion (same lenient host semantics as get_n).
		let numeric = matches!(Kind::of(probe), Kind::Int | Kind::Double)
			&& matches!(kind, Kind::Int | Kind::Double);
		if Kind::of(probe) != kind && !numeric {
			if env::var_os("QUERCUS_OFX_TRACE").is_some() {
				eprintln!("[ofx] property kind mismatch: {name}");
			}
			return Err(status::ERR_UNKNOWN);
		}
	}
	p.values.get(idx(index)?).ok_or(status::ERR_BAD_INDEX)
}

// ---- propSet (single element) ---------------------------------------------

/// Shared implementation of the propSet (single-element) family: write `value`
/// into slot `index` of the property named `name` in `set`.
///
/// An undefined property is **implicitly created** per OFX semantics
/// (ofxProperty.h: "If the property does not exist it is created", same as HS's
/// setValue) — many descriptor properties written during plugin describe
/// (grouping, the capability switches) are not predefined by the host, and
/// refusing to create them would make describe fail outright. A newly created
/// property has dimension `index + 1`, with the leading slots filled with the
/// same `value`; an already defined empty array accepts only `index == 0`
/// (push), because an empty array has no type probe. Ownership of `value` moves
/// into the property set (`Value` owns payloads such as strings) and `index` is
/// the element index.
///
/// # Errors
///
/// Defined but with a mismatched type → kOfxStatErrUnknown; negative index,
/// `index > dimension`, or writing `index > 0` into an empty array →
/// kOfxStatErrBadIndex.
fn set_value(set: &PropertySet, name: &str, index: c_int, value: Value) -> Result<(), c_int> {
	set.with_locked(|props| {
		let idx = idx(index)?;
		let Some(p) = props.iter_mut().find(|p| p.name == name) else {
			// Implicit creation: dimension index+1, leading slots filled with the same value
			// (HS new-property default semantics — a new property first has one default element,
			// then is written slot by slot).
			let mut values = vec![value.clone(); idx];
			values.push(value);
			props.push(crate::property::Property {
				name: name.to_string(),
				values,
			});
			return Ok(());
		};
		// Empty property (a host-predefined empty array, e.g. OfxImageEffectPropSupportedContexts)
		// has no type probe: push directly per HS's index == size append semantics.
		if p.values.is_empty() {
			if idx != 0 {
				if env::var_os("QUERCUS_OFX_TRACE").is_some() {
					eprintln!("[ofx] propSet into empty predefined array: {name}[{idx}]");
				}
				return Err(status::ERR_BAD_INDEX);
			}
			p.values.push(value);
			return Ok(());
		}
		let probe = &p.values[0];
		if Kind::of(probe) != Kind::of(&value) {
			return Err(status::ERR_UNKNOWN);
		}
		// HS `setValue` allows appending when index == size (ofxhPropertySuite.cpp:284)
		// — negotiation properties such as addSupportedContext grow slot by slot exactly this way.
		if idx == p.values.len() {
			p.values.push(value);
			return Ok(());
		}
		if idx >= p.values.len() && env::var_os("QUERCUS_OFX_TRACE").is_some() {
			eprintln!(
				"[ofx] propSet out of range: {name}[{idx}] (dim {})",
				p.values.len()
			);
		}
		let slot = p.values.get_mut(idx).ok_or(status::ERR_BAD_INDEX)?;
		*slot = value;
		Ok(())
	})
}

/// propSetPointer: write the raw pointer `value` into slot `index` of the `name`
/// property.
///
/// The pointer is stored as-is; the suite neither dereferences nor frees it. The
/// property is implicitly created when absent (`index + 1` dimensions, leading
/// slots with the same value); `index == dimension` appends; a larger
/// out-of-bounds index or a negative index → kOfxStatErrBadIndex; defined but not
/// of type Pointer → kOfxStatErrUnknown. `value` may be NULL (a null pointer is
/// a valid property value).
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), and `name` must be a valid NUL-terminated C
/// string.
unsafe extern "C" fn prop_set_pointer(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	value: *mut c_void,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			set_value(set, name, index, Value::Pointer(value))
		})
	}
}

/// propSetString: copy the C string `value` into slot `index` of the `name`
/// property.
///
/// The string is truncated at the first NUL and copied into an internal
/// `CString`; the plugin retains ownership of the original buffer. The property
/// is implicitly created when absent; `index == dimension` appends; a larger
/// out-of-bounds index or a negative index → kOfxStatErrBadIndex. `value` NULL →
/// kOfxStatErrValue; defined but not of type String → kOfxStatErrUnknown.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), and `name` and `value` must be valid
/// NUL-terminated C strings.
unsafe extern "C" fn prop_set_string(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	value: *const c_char,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if value.is_null() {
				return Err(status::ERR_VALUE);
			}
			// C semantics: truncate at the first NUL; CString::new cannot fail
			// (from_ptr already guarantees NUL termination).
			let s = CString::new(CStr::from_ptr(value).to_bytes()).unwrap();
			set_value(set, name, index, Value::String(s))
		})
	}
}

/// propSetDouble: write `value` into slot `index` of the `name` property.
///
/// The property is implicitly created when absent (`index + 1` dimensions,
/// leading slots with the same value); `index == dimension` appends; a larger
/// out-of-bounds index or a negative index → kOfxStatErrBadIndex; defined but not
/// of type Double → kOfxStatErrUnknown (strict typing: an Int property is not
/// written as Double).
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), and `name` must be a valid NUL-terminated C
/// string.
unsafe extern "C" fn prop_set_double(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	value: c_double,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			set_value(set, name, index, Value::Double(value))
		})
	}
}

/// propSetInt: write `value` into slot `index` of the `name` property.
///
/// The property is implicitly created when absent per OFX semantics (`index + 1`
/// dimensions, leading slots with the same value; see [`set_value`]);
/// `index == dimension` appends one element (negotiation properties grow slot by
/// slot), a larger out-of-bounds index or a negative index → kOfxStatErrBadIndex;
/// defined but not of type Int → kOfxStatErrUnknown (propSet is strict and does
/// not convert between Int/Double).
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), and `name` must be a valid NUL-terminated C
/// string.
unsafe extern "C" fn prop_set_int(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	value: c_int,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			set_value(set, name, index, Value::Int(value))
		})
	}
}

// ---- propSetN (batch) -----------------------------------------------------

/// Reads `count` elements from a C array and writes them wholesale; when count
/// != the current dimension it replaces the whole property (HS `setValueN`'s
/// resize semantics, ofxhPropertySuite.cpp:299-311).
fn set_values(
	set: &PropertySet,
	name: &str,
	count: c_int,
	values: Vec<Value>,
) -> Result<(), c_int> {
	set.with_locked(|props| {
		let Some(p) = props.iter_mut().find(|p| p.name == name) else {
			// Same as single-element set_value: undefined properties are implicitly created per OFX semantics.
			props.push(crate::property::Property {
				name: name.to_string(),
				values,
			});
			return Ok(());
		};
		// With count == 0 there is no type to probe (HS still does fetchTypedProperty).
		if let Some(first) = p.values.first() {
			if let Some(v) = values.first() {
				if Kind::of(first) != Kind::of(v) {
					return Err(status::ERR_UNKNOWN);
				}
			}
		}
		let count = idx(count)?;
		if count != p.values.len() {
			// Dimension change: replace wholesale (HS does resize + slot-by-slot write, equivalent).
			p.values = values;
		} else {
			for (i, v) in values.into_iter().enumerate() {
				p.values[i] = v;
			}
		}
		Ok(())
	})
}

/// propSetPointerN: write the `count` raw pointers in `values[0..count]`
/// wholesale into the `name` property.
///
/// The pointer values are stored as-is and ownership remains with the plugin; the
/// property is implicitly created when absent; when `count` differs from the
/// current dimension the property is replaced wholesale, otherwise it is
/// overwritten slot by slot. A first-element type mismatch →
/// kOfxStatErrUnknown; negative `count` → kOfxStatErrBadIndex; `count > 0` with
/// `values` NULL → kOfxStatErrValue.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string,
/// and when `values` is non-null it must be readable for `count` `*mut c_void`
/// elements.
unsafe extern "C" fn prop_set_pointer_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	values: *const *mut c_void,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && values.is_null() {
				return Err(status::ERR_VALUE);
			}
			let vals = (0..idx(count)?)
				.map(|i| Value::Pointer(*values.add(i)))
				.collect();
			set_values(set, name, count, vals)
		})
	}
}

/// propSetStringN: copy the `count` C strings in `values[0..count]` into the
/// `name` property.
///
/// Each string is truncated at the first NUL and **copied** into an internal
/// `CString` (the plugin may reuse the buffer immediately). The property is
/// implicitly created when absent; when `count` differs from the dimension the
/// property is replaced wholesale. Errors: `count > 0` with `values` NULL, or any
/// element NULL → kOfxStatErrValue; first element type mismatch with the existing
/// property → kOfxStatErrUnknown; negative `count` → kOfxStatErrBadIndex.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string,
/// and when `values` is non-null it must be readable for `count` valid
/// NUL-terminated C-string pointers.
unsafe extern "C" fn prop_set_string_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	values: *const *const c_char,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && values.is_null() {
				return Err(status::ERR_VALUE);
			}
			let mut vals = Vec::with_capacity(count.max(0) as usize);
			for i in 0..idx(count)? {
				let v = *values.add(i);
				if v.is_null() {
					return Err(status::ERR_VALUE);
				}
				vals.push(Value::String(
					CString::new(CStr::from_ptr(v).to_bytes()).unwrap(),
				));
			}
			set_values(set, name, count, vals)
		})
	}
}

/// propSetDoubleN: write the `count` doubles in `values[0..count]` wholesale
/// into the `name` property.
///
/// The property is implicitly created when absent; when `count` differs from the
/// current dimension it is replaced wholesale (resize semantics, same as HS
/// `setValueN`), otherwise it is overwritten slot by slot. Note that the propSet
/// family **strictly** distinguishes Int/Double and does not convert values
/// (unlike propGet's leniency): a first-element type mismatch →
/// kOfxStatErrUnknown; negative `count` → kOfxStatErrBadIndex; `count > 0` with
/// `values` NULL → kOfxStatErrValue.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string,
/// and when `values` is non-null it must be readable for `count` `c_double`
/// elements.
unsafe extern "C" fn prop_set_double_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	values: *const c_double,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && values.is_null() {
				return Err(status::ERR_VALUE);
			}
			let vals = (0..idx(count)?)
				.map(|i| Value::Double(*values.add(i)))
				.collect();
			set_values(set, name, count, vals)
		})
	}
}

/// propSetIntN: write the `count` ints in `values[0..count]` wholesale into the
/// `name` property.
///
/// The property is implicitly created when absent per OFX semantics; when `count`
/// differs from the current dimension it is replaced wholesale (HS `setValueN`'s
/// resize semantics, ofxhPropertySuite.cpp:299-311), otherwise it is overwritten
/// slot by slot. A first-element type mismatch (for example writing a Double source
/// into an Int array) → kOfxStatErrUnknown; negative `count` →
/// kOfxStatErrBadIndex; `count > 0` with `values` NULL → kOfxStatErrValue.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string,
/// and when `values` is non-null it must be readable for `count` `c_int`
/// elements.
unsafe extern "C" fn prop_set_int_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	values: *const c_int,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && values.is_null() {
				return Err(status::ERR_VALUE);
			}
			let vals = (0..idx(count)?)
				.map(|i| Value::Int(*values.add(i)))
				.collect();
			set_values(set, name, count, vals)
		})
	}
}

// ---- propGet (single element) ---------------------------------------------

/// Single-element lookup for non-string scalars: hands the caller a clone of the
/// `Value`.
///
/// `set` is the property set to read, `name` is the property name, and `index` is
/// the element index; strings go through [`get_string`]'s in-place pointer path
/// (a cloned `CString` pointer would dangle) and do not pass through here. The
/// caller matches the variant and performs numeric conversion per concrete suite
/// entry.
///
/// # Errors
///
/// Undefined property or type mismatch → kOfxStatErrUnknown; negative index/out of
/// bounds → kOfxStatErrBadIndex.
fn get_scalar(set: &PropertySet, name: &str, index: c_int, kind: Kind) -> Result<Value, c_int> {
	set.with_locked(|props| get_value(props, name, index, kind).cloned())
}

/// propGetPointer: write the raw pointer at `index` of the `name` property into
/// `*out`.
///
/// Undefined property or a first element that is not Pointer →
/// kOfxStatErrUnknown; negative index/out of bounds → kOfxStatErrBadIndex; `out`
/// NULL (before the bounds check) → kOfxStatErrValue. The retrieved pointer
/// belongs to the plugin; the suite does not take over its lifetime.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string,
/// and when `out` is non-null it must be writable for one `*mut c_void`.
unsafe extern "C" fn prop_get_pointer(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	out: *mut *mut c_void,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if out.is_null() {
				return Err(status::ERR_VALUE);
			}
			let v = get_scalar(set, name, index, Kind::Pointer)?;
			match v {
				Value::Pointer(p) => {
					if env::var_os("QUERCUS_OFX_TRACE").is_some() {
						eprintln!("[ofx] propGetPointer(h={handle:p}, {name}[{index}]) -> {p:p}");
					}
					*out = p;
					Ok(())
				}
				_ => Err(status::FAILED),
			}
		})
	}
}

/// Returns the **in-place** C-string pointer of the string at `index` of the
/// `name` property.
///
/// The OFX contract requires a pointer into the host's internal storage: valid
/// until the property is next modified. Returning a pointer to a cloned `CString`
/// would dangle as soon as the caller used it, so this exposes the `as_ptr()` of
/// the `PropertySet` entry directly. `index` is the element index (negative or out
/// of bounds fails); the string value itself is not UTF-8 validated (it is already
/// a valid C string when stored).
///
/// # Errors
///
/// Undefined property or type mismatch → kOfxStatErrUnknown; negative index/out of
/// bounds → kOfxStatErrBadIndex.
fn get_string(set: &PropertySet, name: &str, index: c_int) -> Result<*mut c_char, c_int> {
	set.with_locked(|props| match get_value(props, name, index, Kind::Str)? {
		Value::String(s) => Ok(s.as_ptr() as *mut c_char),
		_ => Err(status::FAILED),
	})
}

/// propGetString: write the **in-place** C-string pointer of the string at
/// `index` of the `name` property into `*out` (see [`get_string`]).
///
/// The pointer points into the property set's internal storage and is valid until
/// the property is next modified; the plugin must not free it or cache it beyond
/// that window. Undefined property/type mismatch → kOfxStatErrUnknown; negative
/// index or out of bounds → kOfxStatErrBadIndex; `out` NULL → kOfxStatErrValue.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string,
/// and when `out` is non-null it must be writable for one `*mut c_char`.
unsafe extern "C" fn prop_get_string(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	out: *mut *mut c_char,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if out.is_null() {
				return Err(status::ERR_VALUE);
			}
			let r = get_string(set, name, index);
			if env::var_os("QUERCUS_OFX_TRACE").is_some() {
				let rendered = match &r {
					Ok(p) => format!("0 <- {:?}", std::ffi::CStr::from_ptr(*p)),
					Err(c) => format!("{c}"),
				};
				eprintln!("[ofx] propGetString(h={handle:p}, {name}[{index}]) -> {rendered}");
			}
			*out = r?;
			Ok(())
		})
	}
}

/// propGetDouble: write the value at `index` of the `name` property into `*out`
/// as a double.
///
/// Int elements are promoted to `f64` automatically (forgiving-host semantics,
/// same as [`get_n`]); undefined property or first-element type mismatch →
/// kOfxStatErrUnknown; negative index/out of bounds → kOfxStatErrBadIndex; `out`
/// NULL → kOfxStatErrValue.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string,
/// and when `out` is non-null it must be writable for one `c_double`.
unsafe extern "C" fn prop_get_double(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	out: *mut c_double,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if out.is_null() {
				return Err(status::ERR_VALUE);
			}
			let v = get_scalar(set, name, index, Kind::Double)?;
			match v {
				Value::Double(d) => {
					*out = d;
					Ok(())
				}
				// Int <-> Double numeric coercion (lenient host semantics, same as get_n).
				Value::Int(i) => {
					*out = i as f64;
					Ok(())
				}
				_ => Err(status::FAILED),
			}
		})
	}
}

/// propGetInt: write the value at `index` of the `name` property into `*out` as
/// an int.
///
/// The type check runs before the index lookup (HS order: `fetchTypedProperty`
/// before `getValueRaw`, ofxhPropertySuite.cpp:787/794/257): undefined property
/// or type mismatch → kOfxStatErrUnknown; negative index or out of bounds →
/// kOfxStatErrBadIndex. Double elements are truncated with `as i32` (not rounded,
/// saturating out of range). A NULL `out` with a valid index returns
/// kOfxStatErrValue.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string,
/// and when `out` is non-null it must be writable for one `c_int`.
unsafe extern "C" fn prop_get_int(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	out: *mut c_int,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if out.is_null() {
				return Err(status::ERR_VALUE);
			}
			let v = get_scalar(set, name, index, Kind::Int)?;
			match v {
				Value::Int(i) => {
					*out = i;
					Ok(())
				}
				// Int <-> Double numeric coercion (lenient host semantics, same as get_n).
				Value::Double(d) => {
					*out = d as i32;
					Ok(())
				}
				_ => Err(status::FAILED),
			}
			.inspect_err(|&c| {
				if env::var_os("QUERCUS_OFX_TRACE").is_some() {
					eprintln!("[ofx] propGetInt({name}) -> {c}");
				}
			})
		})
	}
}

// ---- propGetN (batch) -----------------------------------------------------

/// Shared implementation of `propGet*N`: look up the property by name and call
/// `write(value, i)` for each of the first min(`count`, dimension) elements.
///
/// Only the first element's type is checked (arrays are assumed homogeneous), and
/// Int/Double coerce to each other numerically. `write` may be called fewer than
/// `count` times and the tail of the output array is never touched — this is
/// exactly HS `getValueNRaw`'s (ofxhPropertySuite.cpp:271-280) "truncate without
/// error" semantics. `set` is the property set to read; `kind` is the type kind
/// requested by the plugin; `write` is the caller-supplied write closure (which
/// dereferences the `out` pointer).
///
/// # Errors
///
/// Property missing or an empty array → kOfxStatErrUnknown; first-element type
/// mismatch (not in the numeric kind) → kOfxStatErrUnknown; negative `count` →
/// kOfxStatErrBadIndex.
fn get_n(
	set: &PropertySet,
	name: &str,
	count: c_int,
	kind: Kind,
	write: impl Fn(&Value, usize),
) -> Result<(), c_int> {
	set.with_locked(|props| {
		let p = props
			.iter()
			.find(|p| p.name == name)
			.ok_or(status::ERR_UNKNOWN)?;
		let probe = p.values.first().ok_or(status::ERR_UNKNOWN)?;
		// Int <-> Double coercion (forgiving-host semantics): the CImg
		// framework reads the DOUBLE render window with propGetIntN and
		// must still get the values; strict kind checks turn that into a
		// bogus MissingHostFeature at render time.
		let numeric = matches!(Kind::of(probe), Kind::Int | Kind::Double)
			&& matches!(kind, Kind::Int | Kind::Double);
		if Kind::of(probe) != kind && !numeric {
			return Err(status::ERR_UNKNOWN);
		}
		let n = idx(count)?.min(p.values.len());
		for (i, v) in p.values.iter().take(n).enumerate() {
			write(v, i);
		}
		if env::var_os("QUERCUS_OFX_TRACE").is_some() {
			let dump: Vec<String> = p
				.values
				.iter()
				.take(n)
				.map(|v| match v {
					Value::Int(i) => i.to_string(),
					Value::Double(d) => d.to_string(),
					_ => "?".to_string(),
				})
				.collect();
			eprintln!("[ofx] propGetN({name} x{n}) -> ok [{dump:?}]");
		}
		Ok(())
	})
	.inspect_err(|&code| {
		// The property the plugin asked for and we did not have — the
		// single most useful line when a plugin reports
		// MissingHostFeature (trace-gated, like fetchSuite misses).
		if env::var_os("QUERCUS_OFX_TRACE").is_some() {
			eprintln!("[ofx] property miss: {name} (code {code})");
		}
	})
}

/// propGetPointerN: write the first min(`count`, dimension) raw pointers of the
/// `name` property into `out[0..n]`.
///
/// The pointer values are exactly what propSetPointer stored (ownership is always
/// on the plugin side; the suite does not free them). Delegates to [`get_n`]:
/// undefined/empty property or a first element that is not Pointer →
/// kOfxStatErrUnknown; negative `count` → kOfxStatErrBadIndex; `out` NULL with
/// `count > 0` → kOfxStatErrValue.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string,
/// and `out` must be writable for min(`count`, dimension) `*mut c_void` elements.
unsafe extern "C" fn prop_get_pointer_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	out: *mut *mut c_void,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && out.is_null() {
				return Err(status::ERR_VALUE);
			}
			get_n(set, name, count, Kind::Pointer, |v, i| {
				if let Value::Pointer(p) = v {
					*out.add(i) = *p
				}
			})
		})
	}
}

/// propGetStringN: write the **in-place** C-string pointers of the first
/// min(`count`, dimension) strings of the `name` property into `out[0..n]`.
///
/// The pointers point into the property set's internal storage and are valid until
/// the property is next modified; the plugin must not free them or hold them for
/// long (equivalent to C++ returning `const char**`). Delegates to [`get_n`]:
/// undefined/empty property or a first element that is not String →
/// kOfxStatErrUnknown; negative `count` → kOfxStatErrBadIndex; `out` NULL with
/// `count > 0` → kOfxStatErrValue.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string,
/// and `out` must be writable for min(`count`, dimension) `*mut c_char` elements.
unsafe extern "C" fn prop_get_string_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	out: *mut *mut c_char,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && out.is_null() {
				return Err(status::ERR_VALUE);
			}
			get_n(set, name, count, Kind::Str, |v, i| {
				if let Value::String(s) = v {
					*out.add(i) = s.as_ptr() as *mut c_char
				}
			})
		})
	}
}

/// propGetDoubleN: write the first min(`count`, dimension) double values of the
/// `name` property into `out[0..n]`.
///
/// Delegates to [`get_n`]: Int elements are promoted to `f64` automatically
/// (forgiving-host semantics; the CImg framework reads window parameters with
/// propGetDoubleN); undefined/empty property → kOfxStatErrUnknown; first element
/// not numeric → kOfxStatErrUnknown. A `count` larger than the dimension writes
/// only the dimension's worth without error; negative `count` →
/// kOfxStatErrBadIndex; `out` NULL with `count > 0` → kOfxStatErrValue.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string,
/// and `out` must be writable for min(`count`, dimension) `c_double` elements.
unsafe extern "C" fn prop_get_double_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	out: *mut c_double,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && out.is_null() {
				return Err(status::ERR_VALUE);
			}
			get_n(set, name, count, Kind::Double, |v, i| match v {
				Value::Double(d) => *out.add(i) = *d,
				Value::Int(d) => *out.add(i) = *d as f64,
				_ => {}
			})
		})
	}
}

/// propGetIntN: write the first min(`count`, dimension) int values of the `name`
/// property into `out[0..n]`.
///
/// Delegates to [`get_n`]: undefined or empty property → kOfxStatErrUnknown; a
/// first element that is not Int/Double → kOfxStatErrUnknown; Double elements are
/// truncated to `c_int` (a C-style `as` conversion, not rounding). A `count`
/// larger than the dimension writes only the dimension's worth of elements
/// without error; negative `count` → kOfxStatErrBadIndex; `out` NULL with
/// `count > 0` → kOfxStatErrValue (NULL is allowed when `count == 0`). `out` is
/// allocated by the caller with at least `count` elements.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string,
/// and `out` must be writable for min(`count`, dimension) `c_int` elements.
unsafe extern "C" fn prop_get_int_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	out: *mut c_int,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && out.is_null() {
				return Err(status::ERR_VALUE);
			}
			get_n(set, name, count, Kind::Int, |v, i| match v {
				Value::Int(d) => *out.add(i) = *d,
				Value::Double(d) => *out.add(i) = *d as i32,
				_ => {}
			})
		})
	}
}

// ---- propReset / propGetDimension -----------------------------------------

/// propReset: reset a property to its define-time default — not supported by
/// this crate yet.
///
/// This crate does not keep a snapshot of the defaults, so it **always** returns
/// kOfxStatErrUnsupported (see the module docs; HostSupport performs the real
/// restore, and a silent no-op would hide the gap on the host side). Invalid
/// property name → kOfxStatErrValue; `handle` NULL → kOfxStatErrBadHandle.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), and `name` must be a valid NUL-terminated C
/// string.
unsafe extern "C" fn prop_reset(handle: *mut c_void, name: *const c_char) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			// HS `reset()` restores the defaults from define time
			// (ofxhPropertySuite.cpp:330); this crate does not keep a default-value snapshot,
			// so phase 1 explicitly does not support it (see the module docs).
			let _ = (set, name);
			Err(status::ERR_UNSUPPORTED)
		})
	}
}

/// propGetDimension: write the element count of the `name` property into `*out`.
///
/// Undefined property → kOfxStatErrUnknown; a defined empty array is a valid
/// dimension of 0 (the initial state of negotiation properties such as
/// `OfxImageEffectPropSupportedContexts`) and is not an error. `out` NULL →
/// kOfxStatErrValue.
///
/// # Safety
///
/// `handle` must point to a live `PropertySet` (or a registered object encoded
/// per [`crate::suites::tag`]), `name` must be a valid NUL-terminated C string;
/// when `out` is non-null it must be writable for one `c_int`.
unsafe extern "C" fn prop_get_dimension(
	handle: *mut c_void,
	name: *const c_char,
	out: *mut c_int,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if out.is_null() {
				return Err(status::ERR_VALUE);
			}
			// HS: fetchProperty failure (undefined) → Unknown (ofxhPropertySuite.cpp:992);
			// a defined empty array (e.g. a negotiation property's initial state) is valid dimension 0, not an error.
			let exists = set.with_locked(|props| props.iter().any(|p| p.name == name));
			if !exists {
				if env::var_os("QUERCUS_OFX_TRACE").is_some() {
					let tag = crate::suites::tag::kind(handle);
					eprintln!(
						"[ofx] property miss (dimension): {name} (handle {handle:p} tag {tag})"
					);
				}
				return Err(status::ERR_UNKNOWN);
			}
			*out = set.dimension(name) as c_int;
			Ok(())
		})
	}
}

/// The static function table of OfxPropertySuite v1, initialized once on the
/// first call in the process.
///
/// The returned reference lives as long as the process, and its address can be
/// handed to a plugin as `OfxPropertySuiteV1*`; [`crate::suites::fetch_suite`]
/// uses it to answer `kOfxPropertySuite` requests, and repeated calls return the
/// same instance, so pointers can be compared safely.
pub fn suite_v1() -> &'static PropertySuiteV1 {
	static SUITE: sync::OnceLock<PropertySuiteV1> = sync::OnceLock::new();
	SUITE.get_or_init(|| PropertySuiteV1 {
		set_pointer: prop_set_pointer,
		set_string: prop_set_string,
		set_double: prop_set_double,
		set_int: prop_set_int,
		set_pointer_n: prop_set_pointer_n,
		set_string_n: prop_set_string_n,
		set_double_n: prop_set_double_n,
		set_int_n: prop_set_int_n,
		get_pointer: prop_get_pointer,
		get_string: prop_get_string,
		get_double: prop_get_double,
		get_int: prop_get_int,
		get_pointer_n: prop_get_pointer_n,
		get_string_n: prop_get_string_n,
		get_double_n: prop_get_double_n,
		get_int_n: prop_get_int_n,
		reset: prop_reset,
		get_dimension: prop_get_dimension,
	})
}

#[cfg(test)]
mod tests;
