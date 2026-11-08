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

use std::ffi::{c_char, c_double, c_int, c_void, CStr, CString};

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
	let caller = std::panic::Location::caller();
	let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		if handle.is_null() {
			return status::ERR_BAD_HANDLE;
		}
		let set = unsafe { &*crate::suites::tag::strip(handle) };
		f(set).map_or_else(|code| code, |()| status::OK)
	}))
	.unwrap_or(status::FAILED);
	if code != status::OK && std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
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
	if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
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
	// 首元素代理整条属性的类型（宿主只定义同构数组；HS 是定义期
	// 固定类型，等价）。空数组没有类型探针，跳过类型检查，越界
	// 由下面的索引访问报 BadIndex（HS getValueRaw 语义）。
	if let Some(probe) = p.values.first() {
		// Int <-> Double 数值协随（与 get_n 同款宽容宿主语义）。
		let numeric = matches!(Kind::of(probe), Kind::Int | Kind::Double)
			&& matches!(kind, Kind::Int | Kind::Double);
		if Kind::of(probe) != kind && !numeric {
			if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
				eprintln!("[ofx] property kind mismatch: {name}");
			}
			return Err(status::ERR_UNKNOWN);
		}
	}
	p.values.get(idx(index)?).ok_or(status::ERR_BAD_INDEX)
}

// ---- propSet（单元素）-----------------------------------------------------

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
			// 隐式创建：维度 index+1，前导槽位以同值填充（HS 的新属性
			// 默认值语义——新建属性先有一个默认元素再逐位写）。
			let mut values = vec![value.clone(); idx];
			values.push(value);
			props.push(crate::property::Property {
				name: name.to_string(),
				values,
			});
			return Ok(());
		};
		// 空属性（宿主预定义的空数组，如 OfxImageEffectPropSupportedContexts）
		// 没有类型探针：按 HS 的 index == size 追加语义直接 push。
		if p.values.is_empty() {
			if idx != 0 {
				if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
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
		// HS `setValue` 允许 index == size 时追加（ofxhPropertySuite.cpp:284）
		// ——addSupportedContext 等协商属性正是这样逐位增长的。
		if idx == p.values.len() {
			p.values.push(value);
			return Ok(());
		}
		if idx >= p.values.len() && std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
			eprintln!("[ofx] propSet out of range: {name}[{idx}] (dim {})", p.values.len());
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
			// C 语义：截断到首个 NUL；CString::new 不会失败
			//（from_ptr 已保证 NUL 结尾）。
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

// ---- propSetN（批量）------------------------------------------------------

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
			// 与单元素 set_value 一致：未定义属性按 OFX 语义隐式创建。
			props.push(crate::property::Property {
				name: name.to_string(),
				values,
			});
			return Ok(());
		};
		// count == 0 时无类型可探（HS 仍会做 fetchTypedProperty）。
		if let Some(first) = p.values.first() {
			if let Some(v) = values.first() {
				if Kind::of(first) != Kind::of(v) {
					return Err(status::ERR_UNKNOWN);
				}
			}
		}
		let count = idx(count)?;
		if count != p.values.len() {
			// 维度变化：整体替换（HS 是 resize + 逐位写，等价）。
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

// ---- propGet（单元素）-----------------------------------------------------

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
					if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
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
			if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
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
				// Int <-> Double 数值协随（宽容宿主语义，同 get_n）。
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
				// Int <-> Double 数值协随（宽容宿主语义，同 get_n）。
				Value::Double(d) => {
					*out = d as i32;
					Ok(())
				}
				_ => Err(status::FAILED),
			}
			.inspect_err(|&c| {
				if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
					eprintln!("[ofx] propGetInt({name}) -> {c}");
				}
			})
		})
	}
}

// ---- propGetN（批量）------------------------------------------------------

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
		if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
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
		if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
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
			get_n(set, name, count, Kind::Pointer, |v, i| if let Value::Pointer(p) = v { *out.add(i) = *p })
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
			get_n(set, name, count, Kind::Str, |v, i| if let Value::String(s) = v { *out.add(i) = s.as_ptr() as *mut c_char })
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
			// HS `reset()` 恢复到 define 时的默认值
			// （ofxhPropertySuite.cpp:330）；本 crate 不保存默认值快照，
			// 第 1 期明确不支持（见模块文档）。
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
			// HS: fetchProperty 失败（未定义）→ Unknown（ofxhPropertySuite.cpp:992）；
			// 已定义的空数组（如协商属性的初始状态）是合法维度 0，不是错误。
			let exists = set.with_locked(|props| props.iter().any(|p| p.name == name));
			if !exists {
				if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
					let tag = crate::suites::tag::kind(handle);
					eprintln!("[ofx] property miss (dimension): {name} (handle {handle:p} tag {tag})");
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
	static SUITE: std::sync::OnceLock<PropertySuiteV1> = std::sync::OnceLock::new();
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
mod tests {
	use super::*;

	fn handle(set: &PropertySet) -> *mut c_void {
		set as *const PropertySet as *mut c_void
	}

	fn cs(s: &str) -> CString {
		CString::new(s).unwrap()
	}

	/// `Value` does not implement PartialEq (Pointer cannot be compared) — compare
	/// Debug strings instead.
	fn val_eq(a: &Value, b: &Value) -> bool {
		format!("{a:?}") == format!("{b:?}")
	}

	fn assert_value(set: &PropertySet, name: &str, index: usize, want: &Value) {
		assert!(
			val_eq(set.get(name, index).as_ref().expect("属性应存在"), want),
			"{name}[{index}] 值不符"
		);
	}

	/// Null handle → BadHandle; null / non-UTF-8 property name → ErrValue.
	#[test]
	fn bad_handles_and_bad_names() {
		let s = suite_v1();
		let set = PropertySet::new();
		let name = cs("a");
		let mut iv = 0;
		let mut dv = 0.0;
		let mut sv: *mut c_char = std::ptr::null_mut();
		let mut pv: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.set_int)(std::ptr::null_mut(), name.as_ptr(), 0, 1),
				status::ERR_BAD_HANDLE
			);
			assert_eq!(
				(s.get_int)(std::ptr::null_mut(), name.as_ptr(), 0, &mut iv),
				status::ERR_BAD_HANDLE
			);
			// 空属性名。
			assert_eq!((s.set_int)(handle(&set), std::ptr::null(), 0, 1), status::ERR_VALUE);
			assert_eq!(
				(s.get_int)(handle(&set), std::ptr::null(), 0, &mut iv),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.set_double)(handle(&set), std::ptr::null(), 0, 1.0),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.set_string)(handle(&set), std::ptr::null(), 0, name.as_ptr()),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.get_double)(handle(&set), std::ptr::null(), 0, &mut dv),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.get_string)(handle(&set), std::ptr::null(), 0, &mut sv),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.get_pointer)(handle(&set), std::ptr::null(), 0, &mut pv),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.get_dimension)(handle(&set), std::ptr::null(), &mut iv),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.reset)(handle(&set), std::ptr::null()),
				status::ERR_VALUE
			);
			// 非 UTF-8 属性名（0xFF 后接 NUL）。
			let bad: [c_char; 2] = [0xFFu8 as c_char, 0];
			assert_eq!(
				(s.set_int)(handle(&set), bad.as_ptr(), 0, 1),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.get_int)(handle(&set), bad.as_ptr(), 0, &mut iv),
				status::ERR_VALUE
			);
		}
	}

	/// get on an undefined property → ErrUnknown; after define, an empty array has
	/// dimension 0 and an out-of-bounds read → BadIndex; negative index → BadIndex.
	#[test]
	fn get_missing_empty_and_bounds() {
		let s = suite_v1();
		let set = PropertySet::new();
		let n = cs("a");
		let mut iv = 0;
		unsafe {
			assert_eq!(
				(s.get_int)(handle(&set), n.as_ptr(), 0, &mut iv),
				status::ERR_UNKNOWN
			);
			assert_eq!(
				(s.get_int)(handle(&set), n.as_ptr(), -1, &mut iv),
				status::ERR_UNKNOWN,
				"未定义属性先报 Unknown（HS 先查类型）"
			);
		}
		set.define("a", vec![]);
		unsafe {
			assert_eq!(
				(s.get_int)(handle(&set), n.as_ptr(), 0, &mut iv),
				status::ERR_BAD_INDEX
			);
		}
		set.define("a", vec![Value::Int(3)]);
		unsafe {
			assert_eq!(
				(s.get_int)(handle(&set), n.as_ptr(), -1, &mut iv),
				status::ERR_BAD_INDEX
			);
			assert_eq!(
				(s.get_int)(handle(&set), n.as_ptr(), 1, &mut iv),
				status::ERR_BAD_INDEX
			);
		}
	}

	/// propSet implicit creation: index 2 builds a property of dimension 3 with the
	/// leading slots holding the same value; for a defined array, index ==
	/// dimension appends and out of bounds → BadIndex.
	#[test]
	fn set_creates_and_appends() {
		let s = suite_v1();
		let set = PropertySet::new();
		let n = cs("a");
		unsafe {
			assert_eq!((s.set_int)(handle(&set), n.as_ptr(), 2, 7), 0);
		}
		assert_eq!(set.dimension("a"), 3);
		assert_value(&set, "a", 0, &Value::Int(7));
		assert_value(&set, "a", 2, &Value::Int(7));

		// index == 维度 → 追加（协商属性逐位增长语义）。
		unsafe {
			assert_eq!((s.set_int)(handle(&set), n.as_ptr(), 3, 9), 0);
		}
		assert_eq!(set.dimension("a"), 4);
		assert_value(&set, "a", 3, &Value::Int(9));

		// 越界（跳过追加位）→ BadIndex。
		unsafe {
			assert_eq!(
				(s.set_int)(handle(&set), n.as_ptr(), 9, 1),
				status::ERR_BAD_INDEX
			);
		}
		assert_eq!(set.dimension("a"), 4);
	}

	/// Empty predefined array: index 0 pushes; index > 0 → BadIndex.
	#[test]
	fn set_into_empty_predefined_array() {
		let s = suite_v1();
		let set = PropertySet::new();
		set.define("empty", vec![]);
		let n = cs("empty");
		unsafe {
			assert_eq!(
				(s.set_int)(handle(&set), n.as_ptr(), 1, 1),
				status::ERR_BAD_INDEX
			);
			assert_eq!((s.set_int)(handle(&set), n.as_ptr(), 0, 5), 0);
		}
		assert_value(&set, "empty", 0, &Value::Int(5));
	}

	/// propSet type mismatch → Unknown; numeric Int/Double are not converted to
	/// each other (set is strict).
	#[test]
	fn set_type_mismatch() {
		let s = suite_v1();
		let set = PropertySet::new();
		set.define("i", vec![Value::Int(0)]);
		set.define("d", vec![Value::Double(0.0)]);
		let ni = cs("i");
		let nd = cs("d");
		unsafe {
			assert_eq!(
				(s.set_double)(handle(&set), ni.as_ptr(), 0, 1.0),
				status::ERR_UNKNOWN
			);
			assert_eq!(
				(s.set_int)(handle(&set), nd.as_ptr(), 0, 1),
				status::ERR_UNKNOWN
			);
		}
	}

	/// propGet: Int ↔ Double numeric coercion; a mismatched element type in a
	/// mixed array → Failed.
	#[test]
	fn get_numeric_coercion_and_mixed_arrays() {
		let s = suite_v1();
		let set = PropertySet::new();
		set.define("i", vec![Value::Int(3)]);
		set.define("d", vec![Value::Double(1.5)]);
		let ni = cs("i");
		let nd = cs("d");
		let mut dv = 0.0;
		let mut iv = 0;
		unsafe {
			assert_eq!((s.get_double)(handle(&set), ni.as_ptr(), 0, &mut dv), 0);
		}
		assert_eq!(dv, 3.0);
		unsafe {
			assert_eq!((s.get_int)(handle(&set), nd.as_ptr(), 0, &mut iv), 0);
		}
		assert_eq!(iv, 1);

		// 混合数组（首元素定类型，后续元素类型不符）→ 单元素读 Failed。
		let mix = PropertySet::new();
		mix.define(
			"m",
			vec![Value::Double(1.0), Value::String(cs("x"))],
		);
		let nm = cs("m");
		unsafe {
			assert_eq!(
				(s.get_double)(handle(&mix), nm.as_ptr(), 1, &mut dv),
				status::FAILED
			);
		}
		let mix2 = PropertySet::new();
		mix2.define("m", vec![Value::Int(1), Value::String(cs("x"))]);
		unsafe {
			assert_eq!(
				(s.get_int)(handle(&mix2), nm.as_ptr(), 1, &mut iv),
				status::FAILED
			);
		}
	}

	/// Reads with too few/too many components in the requested type: get_string on
	/// a numeric property → Unknown (type mismatch), get_double on a string
	/// property → Unknown.
	#[test]
	fn get_kind_mismatch() {
		let s = suite_v1();
		let set = PropertySet::new();
		set.define("i", vec![Value::Int(1)]);
		set.define("str", vec![Value::String(cs("s"))]);
		let ni = cs("i");
		let ns = cs("str");
		let mut sv: *mut c_char = std::ptr::null_mut();
		let mut dv = 0.0;
		unsafe {
			assert_eq!(
				(s.get_string)(handle(&set), ni.as_ptr(), 0, &mut sv),
				status::ERR_UNKNOWN
			);
			assert_eq!(
				(s.get_double)(handle(&set), ns.as_ptr(), 0, &mut dv),
				status::ERR_UNKNOWN
			);
		}
	}

	/// propGet with a null out pointer → ErrValue (all four scalar reads).
	#[test]
	fn get_null_out_pointers() {
		let s = suite_v1();
		let set = PropertySet::new();
		set.define("i", vec![Value::Int(1)]);
		set.define("d", vec![Value::Double(1.0)]);
		set.define("s", vec![Value::String(cs("x"))]);
		set.define("p", vec![Value::Pointer(std::ptr::null_mut())]);
		let n = cs("i");
		unsafe {
			assert_eq!(
				(s.get_int)(handle(&set), n.as_ptr(), 0, std::ptr::null_mut()),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.get_double)(handle(&set), n.as_ptr(), 0, std::ptr::null_mut()),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.get_string)(handle(&set), n.as_ptr(), 0, std::ptr::null_mut()),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.get_pointer)(handle(&set), n.as_ptr(), 0, std::ptr::null_mut()),
				status::ERR_VALUE
			);
		}
	}

	/// propGetPointer: writes the pointer successfully; a type mismatch in a mixed
	/// array → Failed.
	#[test]
	fn get_pointer_paths() {
		let s = suite_v1();
		let raw = 0x1234usize as *mut c_void;
		let set = PropertySet::new();
		set.define("p", vec![Value::Pointer(raw)]);
		let np = cs("p");
		let mut out: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.get_pointer)(handle(&set), np.as_ptr(), 0, &mut out), 0);
		}
		assert_eq!(out, raw);

		let mix = PropertySet::new();
		mix.define(
			"p",
			vec![Value::Pointer(raw), Value::Int(1)],
		);
		unsafe {
			assert_eq!(
				(s.get_pointer)(handle(&mix), np.as_ptr(), 1, &mut out),
				status::FAILED
			);
		}
	}

	/// propGetString: writes the in-place pointer with the correct contents; mixed
	/// array → Failed.
	#[test]
	fn get_string_paths() {
		let s = suite_v1();
		let set = PropertySet::new();
		set.define("s", vec![Value::String(cs("hello"))]);
		let ns = cs("s");
		let mut out: *mut c_char = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.get_string)(handle(&set), ns.as_ptr(), 0, &mut out), 0);
			assert_eq!(CStr::from_ptr(out).to_bytes(), b"hello");
		}
		let mix = PropertySet::new();
		mix.define(
			"s",
			vec![Value::String(cs("x")), Value::Int(1)],
		);
		unsafe {
			assert_eq!(
				(s.get_string)(handle(&mix), ns.as_ptr(), 1, &mut out),
				status::FAILED
			);
		}
	}

	/// propSetString: NULL value → ErrValue; truncates at the first NUL.
	#[test]
	fn set_string_null_and_truncation() {
		let s = suite_v1();
		let set = PropertySet::new();
		let n = cs("s");
		unsafe {
			assert_eq!(
				(s.set_string)(handle(&set), n.as_ptr(), 0, std::ptr::null()),
				status::ERR_VALUE
			);
		}
	}

	/// propSetN family: implicit creation, dimension replacement/slot-by-slot
	/// writes, empty arrays and NULL arguments.
	#[test]
	fn set_n_families() {
		let s = suite_v1();
		let set = PropertySet::new();

		// set_int_n：隐式创建 2 维。
		let ni = cs("ints");
		let ivals = [1 as c_int, 2];
		unsafe {
			assert_eq!(
				(s.set_int_n)(handle(&set), ni.as_ptr(), 2, ivals.as_ptr()),
				0
			);
		}
		assert_eq!(set.dimension("ints"), 2);
		assert_value(&set, "ints", 1, &Value::Int(2));

		// count 与现有维度相同 → 逐位覆盖。
		let ivals2 = [7 as c_int, 8];
		unsafe {
			assert_eq!(
				(s.set_int_n)(handle(&set), ni.as_ptr(), 2, ivals2.as_ptr()),
				0
			);
		}
		assert_value(&set, "ints", 0, &Value::Int(7));

		// 维度变化 → 整体替换。
		let ivals3 = [9 as c_int];
		unsafe {
			assert_eq!(
				(s.set_int_n)(handle(&set), ni.as_ptr(), 1, ivals3.as_ptr()),
				0
			);
		}
		assert_eq!(set.dimension("ints"), 1);

		// count 0 + NULL：合法（空维度替换）。
		unsafe {
			assert_eq!(
				(s.set_int_n)(handle(&set), ni.as_ptr(), 0, std::ptr::null()),
				0
			);
		}
		assert_eq!(set.dimension("ints"), 0);

		// count > 0 + NULL → ErrValue；负 count → BadIndex。
		unsafe {
			assert_eq!(
				(s.set_int_n)(handle(&set), ni.as_ptr(), 1, std::ptr::null()),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.set_int_n)(handle(&set), ni.as_ptr(), -1, ivals.as_ptr()),
				status::ERR_BAD_INDEX
			);
		}

		// set_double_n：隐式创建 + NULL 分支。
		let nd = cs("dbls");
		let dvals = [1.5 as c_double, 2.5];
		unsafe {
			assert_eq!(
				(s.set_double_n)(handle(&set), nd.as_ptr(), 2, dvals.as_ptr()),
				0
			);
			assert_eq!(
				(s.set_double_n)(handle(&set), nd.as_ptr(), 1, std::ptr::null()),
				status::ERR_VALUE
			);
		}
		assert_value(&set, "dbls", 1, &Value::Double(2.5));

		// set_pointer_n：单元素与 count 0 + NULL。
		let np = cs("ptrs");
		let pvals = [0x55usize as *mut c_void];
		unsafe {
			assert_eq!(
				(s.set_pointer_n)(handle(&set), np.as_ptr(), 1, pvals.as_ptr()),
				0
			);
			assert_eq!(
				(s.set_pointer_n)(handle(&set), np.as_ptr(), 1, std::ptr::null()),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.set_pointer_n)(handle(&set), np.as_ptr(), 0, std::ptr::null()),
				0
			);
		}
		assert_eq!(set.dimension("ptrs"), 0);

		// set_string_n：成功、NULL 数组、数组内 NULL 元素。
		let nstr = cs("strs");
		let sa = cs("a");
		let sb = cs("b");
		let svals = [sa.as_ptr(), sb.as_ptr()];
		unsafe {
			assert_eq!(
				(s.set_string_n)(handle(&set), nstr.as_ptr(), 2, svals.as_ptr()),
				0
			);
			assert_eq!(
				(s.set_string_n)(handle(&set), nstr.as_ptr(), 1, std::ptr::null()),
				status::ERR_VALUE
			);
			let with_null = [sa.as_ptr(), std::ptr::null()];
			assert_eq!(
				(s.set_string_n)(handle(&set), nstr.as_ptr(), 2, with_null.as_ptr()),
				status::ERR_VALUE
			);
		}
		assert_eq!(set.dimension("strs"), 2);
	}

	/// set_n type mismatch → Unknown; an existing array is replaced with empty when
	/// count is 0.
	#[test]
	fn set_n_type_mismatch_and_empty_replace() {
		let s = suite_v1();
		let set = PropertySet::new();
		set.define("i", vec![Value::Int(1)]);
		let ni = cs("i");
		let dvals = [2.0 as c_double];
		unsafe {
			assert_eq!(
				(s.set_double_n)(handle(&set), ni.as_ptr(), 1, dvals.as_ptr()),
				status::ERR_UNKNOWN
			);
			// values 为空（count 0）→ 无类型探针，整体替换为空。
			assert_eq!(
				(s.set_double_n)(handle(&set), ni.as_ptr(), 0, dvals.as_ptr()),
				0
			);
		}
		assert_eq!(set.dimension("i"), 0);
	}

	/// propGetN: Int ↔ Double coercion, min(count, dim) truncation, mixed-array
	/// writes skip mismatched elements.
	#[test]
	fn get_n_families() {
		let s = suite_v1();
		let set = PropertySet::new();
		set.define("ints", vec![Value::Int(1), Value::Int(2)]);
		set.define("dbls", vec![Value::Double(1.5), Value::Double(2.5)]);

		// get_double_n 读 Int 属性 → 协随为 f64。
		let ni = cs("ints");
		let mut dout = [0.0 as c_double; 2];
		unsafe {
			assert_eq!(
				(s.get_double_n)(handle(&set), ni.as_ptr(), 2, dout.as_mut_ptr()),
				0
			);
		}
		assert_eq!(dout, [1.0, 2.0]);

		// get_int_n 读 Double 属性 → 截断为 i32。
		let nd = cs("dbls");
		let mut iout = [0 as c_int; 2];
		unsafe {
			assert_eq!(
				(s.get_int_n)(handle(&set), nd.as_ptr(), 2, iout.as_mut_ptr()),
				0
			);
		}
		assert_eq!(iout, [1, 2]);

		// count > 维度：只拷贝 min(count, dim)。
		let mut one = [0 as c_int; 1];
		unsafe {
			assert_eq!(
				(s.get_int_n)(handle(&set), ni.as_ptr(), 5, one.as_mut_ptr()),
				0
			);
		}
		assert_eq!(one, [1]);

		// count 0：不写。
		let mut zero = [7 as c_int; 1];
		unsafe {
			assert_eq!(
				(s.get_int_n)(handle(&set), ni.as_ptr(), 0, zero.as_mut_ptr()),
				0
			);
		}
		assert_eq!(zero, [7]);

		// get_pointer_n / get_string_n 成功路径。
		let ptrs = PropertySet::new();
		let raw = 0x99usize as *mut c_void;
		ptrs.define("p", vec![Value::Pointer(raw)]);
		let np = cs("p");
		let mut pout = [std::ptr::null_mut(); 1];
		unsafe {
			assert_eq!(
				(s.get_pointer_n)(handle(&ptrs), np.as_ptr(), 1, pout.as_mut_ptr()),
				0
			);
		}
		assert_eq!(pout[0], raw);

		let strs = PropertySet::new();
		strs.define("s", vec![Value::String(cs("hi"))]);
		let ns = cs("s");
		let mut sout = [std::ptr::null_mut(); 1];
		unsafe {
			assert_eq!(
				(s.get_string_n)(handle(&strs), ns.as_ptr(), 1, sout.as_mut_ptr()),
				0
			);
			assert_eq!(CStr::from_ptr(sout[0]).to_bytes(), b"hi");
		}
	}

	/// propGetN error paths: null out, undefined, empty array, type mismatch,
	/// negative count.
	#[test]
	fn get_n_error_paths() {
		let s = suite_v1();
		let set = PropertySet::new();
		set.define("ints", vec![Value::Int(1)]);
		set.define("strs", vec![Value::String(cs("s"))]);
		set.define("empty", vec![]);
		let ni = cs("ints");
		let ns = cs("strs");
		let ne = cs("empty");
		let nmissing = cs("missing");
		let mut iout = [0 as c_int; 2];
		let mut dout = [0.0 as c_double; 2];
		let mut pout = [std::ptr::null_mut(); 2];
		let mut sout = [std::ptr::null_mut(); 2];
		unsafe {
			// 空 out（count > 0）。
			assert_eq!(
				(s.get_int_n)(handle(&set), ni.as_ptr(), 1, std::ptr::null_mut()),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.get_double_n)(handle(&set), ni.as_ptr(), 1, std::ptr::null_mut()),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.get_pointer_n)(handle(&set), ni.as_ptr(), 1, std::ptr::null_mut()),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.get_string_n)(handle(&set), ni.as_ptr(), 1, std::ptr::null_mut()),
				status::ERR_VALUE
			);
			// 未定义 → Unknown；空数组 → Unknown（无类型探针）。
			assert_eq!(
				(s.get_int_n)(handle(&set), nmissing.as_ptr(), 1, iout.as_mut_ptr()),
				status::ERR_UNKNOWN
			);
			assert_eq!(
				(s.get_int_n)(handle(&set), ne.as_ptr(), 1, iout.as_mut_ptr()),
				status::ERR_UNKNOWN
			);
			// 类型不符 → Unknown（数值族之间协随，其余严格）。
			assert_eq!(
				(s.get_int_n)(handle(&set), ns.as_ptr(), 1, iout.as_mut_ptr()),
				status::ERR_UNKNOWN
			);
			assert_eq!(
				(s.get_double_n)(handle(&set), ns.as_ptr(), 1, dout.as_mut_ptr()),
				status::ERR_UNKNOWN
			);
			assert_eq!(
				(s.get_pointer_n)(handle(&set), ni.as_ptr(), 1, pout.as_mut_ptr()),
				status::ERR_UNKNOWN
			);
			assert_eq!(
				(s.get_string_n)(handle(&set), ni.as_ptr(), 1, sout.as_mut_ptr()),
				status::ERR_UNKNOWN
			);
			// 负 count → BadIndex（在 min 之前 idx 转换失败）。
			assert_eq!(
				(s.get_int_n)(handle(&set), ni.as_ptr(), -1, iout.as_mut_ptr()),
				status::ERR_BAD_INDEX
			);
		}
	}

	/// propReset is explicitly unsupported; propGetDimension with null
	/// out/undefined/empty array.
	#[test]
	fn reset_and_dimension() {
		let s = suite_v1();
		let set = PropertySet::new();
		set.define("a", vec![Value::Int(1)]);
		set.define("empty", vec![]);
		let na = cs("a");
		let ne = cs("empty");
		let nmissing = cs("missing");
		let mut dim = -1;
		unsafe {
			assert_eq!(
				(s.reset)(handle(&set), na.as_ptr()),
				status::ERR_UNSUPPORTED
			);
			assert_eq!(
				(s.get_dimension)(handle(&set), na.as_ptr(), std::ptr::null_mut()),
				status::ERR_VALUE
			);
			assert_eq!(
				(s.get_dimension)(handle(&set), nmissing.as_ptr(), &mut dim),
				status::ERR_UNKNOWN
			);
			assert_eq!((s.get_dimension)(handle(&set), na.as_ptr(), &mut dim), 0);
			assert_eq!(dim, 1);
			assert_eq!((s.get_dimension)(handle(&set), ne.as_ptr(), &mut dim), 0);
			assert_eq!(dim, 0, "已定义空数组是合法维度 0");
		}
	}

	/// Diagnostic branches when QUERCUS_OFX_TRACE is enabled: every trace print
	/// site is reached (error paths and success paths). The test serializes itself
	/// and restores the environment variable.
	#[test]
	fn trace_diagnostic_branches() {
		static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
		let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
		let prev = std::env::var_os("QUERCUS_OFX_TRACE");
		// set_var/remove_var 在本 crate 的 2021 edition 中是安全函数。
		std::env::set_var("QUERCUS_OFX_TRACE", "1");
		let body = std::panic::catch_unwind(|| {
			let s = suite_v1();
			let set = PropertySet::new();
			let ni = cs("i");
			let nstr = cs("str");
			let nmissing = cs("missing");
			set.define("i", vec![Value::Int(1)]);
			set.define("str", vec![Value::String(cs("s"))]);
			let mut iv = 0;
			let mut dv = 0.0;
			let mut sv: *mut c_char = std::ptr::null_mut();
			let mut pv: *mut c_void = std::ptr::null_mut();
			let mut dim = 0;
			unsafe {
				// caught 的 trace：非 OK 状态码。
				assert_eq!(
					(s.get_int)(handle(&set), nmissing.as_ptr(), 0, &mut iv),
					status::ERR_UNKNOWN
				);
				// get_value trace：成功 + 失败。
				assert_eq!((s.get_int)(handle(&set), ni.as_ptr(), 0, &mut iv), 0);
				assert_eq!(
					(s.get_int)(handle(&set), nmissing.as_ptr(), 0, &mut iv),
					status::ERR_UNKNOWN
				);
				// 类型不符 trace。
				assert_eq!(
					(s.get_string)(handle(&set), ni.as_ptr(), 0, &mut sv),
					status::ERR_UNKNOWN
				);
				// set_value：空数组越界 trace；越界 trace。
				let empty = PropertySet::new();
				empty.define("e", vec![]);
				let ne = cs("e");
				assert_eq!(
					(s.set_int)(handle(&empty), ne.as_ptr(), 1, 1),
					status::ERR_BAD_INDEX
				);
				assert_eq!(
					(s.set_int)(handle(&set), ni.as_ptr(), 5, 2),
					status::ERR_BAD_INDEX
				);
				// propGetPointer trace（成功）。
				let ptrs = PropertySet::new();
				ptrs.define("p", vec![Value::Pointer(0x11usize as *mut c_void)]);
				let np = cs("p");
				assert_eq!((s.get_pointer)(handle(&ptrs), np.as_ptr(), 0, &mut pv), 0);
				// propGetString trace：成功 + 失败。
				assert_eq!((s.get_string)(handle(&set), nstr.as_ptr(), 0, &mut sv), 0);
				assert_eq!(
					(s.get_string)(handle(&set), nmissing.as_ptr(), 0, &mut sv),
					status::ERR_UNKNOWN
				);
				// propGetInt inspect_err trace。
				assert_eq!(
					(s.get_int)(handle(&set), nmissing.as_ptr(), 0, &mut iv),
					status::ERR_UNKNOWN
				);
				// get_n trace：成功 dump + 未命中 inspect_err。
				let mut iout = [0 as c_int; 2];
				assert_eq!(
					(s.get_int_n)(handle(&set), ni.as_ptr(), 2, iout.as_mut_ptr()),
					0
				);
				assert_eq!(
					(s.get_int_n)(handle(&set), nmissing.as_ptr(), 1, iout.as_mut_ptr()),
					status::ERR_UNKNOWN
				);
				// dump 的 Double 与未知元素分支。
				let mixed = PropertySet::new();
				mixed.define(
					"m",
					vec![Value::Double(1.5), Value::String(cs("x"))],
				);
				let nm = cs("m");
				let mut dout = [0.0 as c_double; 2];
				assert_eq!(
					(s.get_double_n)(handle(&mixed), nm.as_ptr(), 2, dout.as_mut_ptr()),
					0
				);
				// 空 out 的 get_double trace 分支（Err 路径渲染）。
				assert_eq!(
					(s.get_double)(handle(&set), ni.as_ptr(), 0, &mut dv),
					0
				);
				// propGetDimension 未定义 trace。
				assert_eq!(
					(s.get_dimension)(handle(&set), nmissing.as_ptr(), &mut dim),
					status::ERR_UNKNOWN
				);
			}
		});
		match prev {
			Some(v) => std::env::set_var("QUERCUS_OFX_TRACE", v),
			None => std::env::remove_var("QUERCUS_OFX_TRACE"),
		}
		if let Err(p) = body {
			std::panic::resume_unwind(p);
		}
	}
}
