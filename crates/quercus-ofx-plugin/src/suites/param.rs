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

//! OfxParameterSuite v1: parameter definition and get/set access.
//!
//! Semantics follow HS: ofxhParam.cpp:
//! - paramDefine: during describe, attaches the parameter definition to the
//!   effect descriptor (undefined type -> kOfxStatErrUnsupported,
//!   HS:1665-1710; parametric types are allowed, and their curve access goes
//!   through OfxParametricParameterSuite, see suites/parametric.rs);
//! - paramGetHandle: lookup by name (not found -> kOfxStatErrUnknown, HS:1758);
//! - the variadic entry points such as paramGetValue/paramSetValue live in the
//!   C shim (cbits/ofx_param_shim.c): they resolve va_args by
//!   [`crate::param::ParamKind`] and forward to this module's
//!   `ofx_param_*_impl`;
//! - a successful paramSetValue fires instanceChanged (kOfxChangePluginEdited,
//!   `paramChangedByPlugin` at HS:1982-1994), writing back to the bound node
//!   through [`crate::param::notify_instance_changed`];
//! - phase 1 has no animation: AtTime == the current value; keys are always 0;
//!   derivative/integral -> kOfxStatErrMissingHostFeature; editBegin/End are
//!   the edit transaction brackets (undo grouping: write-backs inside the
//!   transaction are merged into one multi command, see
//!   [`crate::instance::Instance::edit_begin`]).

use std::collections::HashMap;
use std::ffi::{c_char, c_double, c_int, c_void, CStr, CString};
use std::sync::Mutex;

use crate::descriptor::EffectDescriptor;
use crate::instance::Instance;
use crate::param::{ChangeReason, ParamDef, ParamInstance, ParamKind, ParamValue};
use crate::suites::status;
use crate::suites::tag;

/// Function table layout (identical to the SDK's `OfxParameterSuiteV1`; field
/// order follows the SDK).
#[repr(C)]
pub struct ParameterSuiteV1 {
	/// paramDefine (during describe): defines a parameter.
	pub param_define:
		unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *mut *mut c_void) -> c_int,
	/// paramGetHandle
	pub param_get_handle: unsafe extern "C" fn(
		*mut c_void,
		*const c_char,
		*mut *mut c_void,
		*mut *mut c_void,
	) -> c_int,
	/// paramSetGetPropertySet
	pub param_set_get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
	/// paramGetPropertySet
	pub param_get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
	/// paramGetValue (variadic out-parameters are interpreted by type; the
	/// entry point is in the C shim)
	pub param_get_value: unsafe extern "C" fn(*mut c_void, ...) -> c_int,
	/// paramGetValueAtTime
	pub param_get_value_at_time: unsafe extern "C" fn(*mut c_void, c_double, ...) -> c_int,
	/// paramGetDerivative
	pub param_get_derivative: unsafe extern "C" fn(*mut c_void, c_double, ...) -> c_int,
	/// paramGetIntegral
	pub param_get_integral: unsafe extern "C" fn(*mut c_void, c_double, c_double, ...) -> c_int,
	/// paramSetValue (fires instanceChanged)
	pub param_set_value: unsafe extern "C" fn(*mut c_void, ...) -> c_int,
	/// paramSetValueAtTime
	pub param_set_value_at_time: unsafe extern "C" fn(*mut c_void, c_double, ...) -> c_int,
	/// paramGetNumKeys
	pub param_get_num_keys: unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int,
	/// paramGetKeyTime
	pub param_get_key_time: unsafe extern "C" fn(*mut c_void, c_int, *mut c_double) -> c_int,
	/// paramGetKeyIndex
	pub param_get_key_index:
		unsafe extern "C" fn(*mut c_void, c_double, c_int, *mut c_int) -> c_int,
	/// paramDeleteKey
	pub param_delete_key: unsafe extern "C" fn(*mut c_void, c_double) -> c_int,
	/// paramDeleteAllKeys
	pub param_delete_all_keys: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// paramCopy
	pub param_copy:
		unsafe extern "C" fn(*mut c_void, *mut c_void, c_double, c_double, *const c_void) -> c_int,
	/// paramEditBegin / paramEditEnd (edit transaction brackets)
	pub param_edit_begin: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// paramEditEnd
	pub param_edit_end: unsafe extern "C" fn(*mut c_void) -> c_int,
}

// ---- 句柄解析 -----------------------------------------------------------

/// param-set handle: at describe time this is the effect descriptor, at
/// instance time the instance (getParamSet returns the same handle as the
/// effect, see the image_effect suite).
enum ParamSetRef<'a> {
	Descriptor(&'a mut EffectDescriptor),
	Instance(&'a Instance),
}

/// param handle: at describe time the definition (value = default), at
/// instance time the instance.
enum ParamRef<'a> {
	Def(&'a ParamDef),
	Instance(&'a ParamInstance),
}

/// Resolve a param-set (effect) handle: the effect descriptor during describe,
/// the effect instance at instance time.
///
/// A handle is the object's `props` address plus a low 3-bit tag; a null
/// pointer, or a tag that is neither `DESCRIPTOR` nor `INSTANCE`, yields
/// `kOfxStatErrBadHandle`. The returned `'static` reference is an extended
/// borrow of the raw pointer, so the caller must keep the object alive for the
/// duration of the borrow.
fn resolve_param_set(handle: *mut c_void) -> Result<ParamSetRef<'static>, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	// 两种对象 props 均在偏移 0（句柄约定）。
	unsafe {
		match tag::kind(handle) {
			tag::DESCRIPTOR => Ok(ParamSetRef::Descriptor(
				&mut *(tag::strip(handle) as *mut EffectDescriptor),
			)),
			tag::INSTANCE => Ok(ParamSetRef::Instance(
				&*(tag::strip(handle) as *const Instance),
			)),
			_ => Err(status::ERR_BAD_HANDLE),
		}
	}
}

/// Resolve a param handle: at describe time the parameter definition
/// ([`ParamRef::Def`]), at instance time the parameter instance
/// ([`ParamRef::Instance`]).
///
/// A handle is the object's `props` address plus a low 3-bit tag; a null
/// pointer, or a tag that is not `PARAM_DEF`/`PARAM_INSTANCE`, yields
/// `kOfxStatErrBadHandle`. The returned `'static` reference is an extended
/// borrow of the raw pointer, so the caller must keep the object alive for the
/// duration of the borrow; every entry point of the parameter suite goes
/// through here first.
fn resolve_param(handle: *mut c_void) -> Result<ParamRef<'static>, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	unsafe {
		match tag::kind(handle) {
			tag::PARAM_DEF => Ok(ParamRef::Def(&*(tag::strip(handle) as *const ParamDef))),
			tag::PARAM_INSTANCE => Ok(ParamRef::Instance(
				&*(tag::strip(handle) as *const ParamInstance),
			)),
			_ => Err(status::ERR_BAD_HANDLE),
		}
	}
}

/// Convert a C string argument (property names, parameter names, ...) to
/// `&str`.
///
/// A null pointer or non-UTF-8 content returns `kOfxStatErrValue`; the returned
/// `&str` borrows the memory `name` points to, with a lifetime `'a` chosen by
/// the caller and not tied to the argument.
///
/// # Safety
///
/// `name` must point to a readable NUL-terminated C string that stays valid for
/// as long as the returned borrow is used.
unsafe fn c_name<'a>(name: *const c_char) -> Result<&'a str, c_int> {
	if name.is_null() {
		return Err(status::ERR_VALUE);
	}
	unsafe { CStr::from_ptr(name) }
		.to_str()
		.map_err(|_| status::ERR_VALUE)
}

/// Shared entry-point template: panic fallback.
#[track_caller]
/// Unified template for all C entry points: runs `f` inside `catch_unwind` and
/// normalizes the result to an OFX status code.
///
/// `f` returning `Ok(())` -> `kOfxStatOK`; `Err(code)` -> that status code
/// passed through unchanged; a panic -> `kOfxStatFailed` (an unwind never
/// crosses the C ABI). When the environment variable `QUERCUS_OFX_TRACE` is
/// set, non-OK codes and the call site are printed to stderr; `#[track_caller]`
/// makes the location point at the real entry point rather than this template.
fn caught(f: impl FnOnce() -> Result<(), c_int>) -> c_int {
	let caller = std::panic::Location::caller();
	let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_or_else(
		|_| status::FAILED,
		|r| r.map_or_else(|c| c, |()| status::OK),
	);
	if std::env::var_os("QUERCUS_OFX_TRACE").is_some()
		&& code != status::OK {
			eprintln!("[ofx] param suite error {code} at {caller}");
		}
	code
}

// ---- 变长参数实现（C shim 转发）----------------------------------------

impl ParamKind {
	/// Convert the integer type tag passed in by the C shim into [`ParamKind`].
	///
	/// 1..=12 correspond to Int, Int2, Int3, Double, Double2, Double3, Bool,
	/// Choice, Rgb, Rgba, Str, StrChoice in that order (matching the numbering
	/// convention in `cbits/ofx_param_shim.c`); any other value returns `None`,
	/// and the caller turns that into `kOfxStatErrUnknown`.
	fn from_i32(v: c_int) -> Option<ParamKind> {
		Some(match v {
			1 => ParamKind::Int,
			2 => ParamKind::Int2,
			3 => ParamKind::Int3,
			4 => ParamKind::Double,
			5 => ParamKind::Double2,
			6 => ParamKind::Double3,
			7 => ParamKind::Bool,
			8 => ParamKind::Choice,
			9 => ParamKind::Rgb,
			10 => ParamKind::Rgba,
			11 => ParamKind::Str,
			12 => ParamKind::StrChoice,
			_ => return None,
		})
	}
}

/// Write the numeric components of [`ParamValue`] into the C-side typed output
/// buffer according to `kind`.
///
/// `out` is interpreted by `kind`: `Int` -> `c_int`, `Int2`/`Int3` ->
/// `c_int[2|3]`, `Double` -> `f64`, `Double2`/`Double3` -> `f64[2|3]`,
/// `Bool`/`Choice` -> `c_int`, `Rgb`/`Rgba` -> `f64[3|4]`; the caller must
/// guarantee that the pointer is aligned and long enough. After the write the
/// memory is owned by the caller.
///
/// # Errors
///
/// Returns `kOfxStatFailed` when the value variant does not match `kind`, or
/// when `kind` is a string kind (strings go through
/// [`ofx_param_get_string_impl`]).
fn write_value(v: &ParamValue, kind: ParamKind, out: *mut c_void) -> Result<(), c_int> {
	let err = || Err(status::FAILED);
	unsafe {
		match kind {
			ParamKind::Int => match v {
				ParamValue::Int(a, _) => {
					*(out as *mut c_int) = a[0];
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Int2 | ParamKind::Int3 => match v {
				ParamValue::Int(a, dim) => {
					let n = if kind == ParamKind::Int2 { 2 } else { 3 };
					let dst = std::slice::from_raw_parts_mut(out as *mut c_int, n);
					for (d, s) in dst.iter_mut().zip(a.iter()).take(n) {
						*d = *s;
					}
					let _ = dim;
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Double => match v {
				ParamValue::Double(d, _) => {
					*(out as *mut f64) = d[0];
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Double2 | ParamKind::Double3 => match v {
				ParamValue::Double(d, _) => {
					let n = if kind == ParamKind::Double2 { 2 } else { 3 };
					let dst = std::slice::from_raw_parts_mut(out as *mut f64, n);
					for (dd, s) in dst.iter_mut().zip(d.iter()).take(n) {
						*dd = *s;
					}
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Bool => match v {
				ParamValue::Bool(b) => {
					*(out as *mut c_int) = *b as c_int;
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Choice => match v {
				ParamValue::Choice(c) => {
					*(out as *mut c_int) = *c;
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Rgb | ParamKind::Rgba => match v {
				ParamValue::Color(c, _) => {
					let n = if kind == ParamKind::Rgb { 3 } else { 4 };
					let dst = std::slice::from_raw_parts_mut(out as *mut f64, n);
					for (dd, s) in dst.iter_mut().zip(c.iter()).take(n) {
						*dd = *s;
					}
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Str | ParamKind::StrChoice => {
				// 字符串走 ofx_param_get_string_impl（内驻指针）。
				err()
			}
		}
	}
}

/// Read a [`ParamValue`] (numeric kinds) from the C-side typed input buffer
/// according to `kind`.
///
/// `input` is interpreted by `kind` as a scalar or a multi-component array;
/// missing components are filled with 0 (e.g. `Int` -> `[v, 0, 0]`, dim=1).
/// `Bool` treats non-zero as true, `Choice` takes the integer directly. String
/// kinds (`Str`/`StrChoice`) return `None` and are handled by
/// [`ofx_param_set_string_impl`]. The caller must guarantee that `input` is
/// aligned and long enough: `Int3`/`Double3` read 3 components, `Rgba` reads 4.
fn read_value(kind: ParamKind, input: *const c_void) -> Option<ParamValue> {
	unsafe {
		match kind {
			ParamKind::Int => Some(ParamValue::Int([*(input as *const c_int), 0, 0], 1)),
			ParamKind::Int2 => {
				let a = *(input as *const [c_int; 2]);
				Some(ParamValue::Int([a[0], a[1], 0], 2))
			}
			ParamKind::Int3 => {
				let a = *(input as *const [c_int; 3]);
				Some(ParamValue::Int([a[0], a[1], a[2]], 3))
			}
			ParamKind::Double => Some(ParamValue::Double([*(input as *const f64), 0.0, 0.0], 1)),
			ParamKind::Double2 => {
				let a = *(input as *const [f64; 2]);
				Some(ParamValue::Double([a[0], a[1], 0.0], 2))
			}
			ParamKind::Double3 => {
				let a = *(input as *const [f64; 3]);
				Some(ParamValue::Double([a[0], a[1], a[2]], 3))
			}
			ParamKind::Bool => Some(ParamValue::Bool(*(input as *const c_int) != 0)),
			ParamKind::Choice => Some(ParamValue::Choice(*(input as *const c_int))),
			ParamKind::Rgb => {
				let a = *(input as *const [f64; 3]);
				Some(ParamValue::Color([a[0], a[1], a[2], 0.0], 3))
			}
			ParamKind::Rgba => {
				let a = *(input as *const [f64; 4]);
				Some(ParamValue::Color([a[0], a[1], a[2], a[3]], 4))
			}
			ParamKind::Str | ParamKind::StrChoice => None,
		}
	}
}

/// Fetch the parameter's "current value": at instance time reads
/// [`ParamInstance::get`], at describe time reads the default from the parameter
/// definition ([`ParamDef::default`]).
///
/// The return value is a cloned copy owned by the caller; describe time
/// therefore has no mutable state to write.
fn value_of(r: &ParamRef) -> ParamValue {
	match r {
		ParamRef::Instance(p) => p.get(),
		ParamRef::Def(d) => d.default.clone(),
	}
}

/// param -> owning instance write-back lookup table (used by instanceChanged).
/// `// TODO(instance)`: registered by the host after createInstance, removed on
/// destroy.
static PARAM_OWNER: std::sync::LazyLock<Mutex<HashMap<usize, usize>>> =
	std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Register a parameter -> instance mapping (called on the instantiation path).
pub(crate) fn register_param_owner(param_props: usize, instance_props: usize) {
	PARAM_OWNER
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(param_props, instance_props);
}

/// Remove all parameter registrations of one instance (called on the destroy
/// path).
pub(crate) fn unregister_params_of(instance_props: usize) {
	PARAM_OWNER
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.retain(|_, owner| *owner != instance_props);
}

/// instanceChanged trigger after a successful paramSetValue (HS:
/// ofxhParam.cpp:1991-1994 `paramChangedByPlugin`). A no-op when the instance
/// is not registered (the unbound-node case is already a no-op). The parametric
/// suite's Set/Add/Delete reuse this path (pub(crate): suites/parametric.rs).
pub(crate) fn notify_changed(param_props: usize, name: &str) {
	let owner = PARAM_OWNER
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&param_props)
		.copied();
	if let Some(inst_addr) = owner {
		// instance 的 props 在偏移 0（句柄约定）。
		let inst = unsafe { &*(inst_addr as *const Instance) };
		crate::param::notify_instance_changed(inst, name, ChangeReason::PluginEdited);
	}
}

/// Parameter type query (called by the C shim): unknown/invalid handle -> 0
/// (dispatch failure).
#[no_mangle]
/// Query the [`ParamKind`] of a parameter handle (used by the C shim for type
/// dispatch).
///
/// Returns the integer encoding of the kind (1..=12, encoding in
/// [`ParamKind::from_i32`]); for an empty/invalid handle or an unknown type it
/// returns 0, which the C shim treats as a dispatch failure. Panics are also
/// swallowed and treated as 0, so this entry point never unwinds into C.
///
/// # Safety
///
/// `param` must be a valid param handle (definition or instance) or a null
/// pointer, and the handle must stay valid for the duration of the call.
pub unsafe extern "C" fn ofx_param_kind_of(param: *mut c_void) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		match resolve_param(param) {
			Ok(r) => match &r {
				ParamRef::Instance(p) => p.def.kind().map_or(0, |k| k as c_int),
				ParamRef::Def(d) => d.kind().map_or(0, |k| k as c_int),
			},
			Err(_) => 0,
		}
	}))
	.unwrap_or(0)
}

/// Numeric get (forwarded by the C shim): `out` is a typed array for `kind`.
#[no_mangle]
/// Numeric parameter read (the C shim's `paramGetValue*` forward target).
///
/// `out` is a C-side typed array allocated for `kind` (mapping in
/// [`write_value`]); the caller guarantees that it is writable and long enough.
/// On success the current value (the instance value, or the describe-time
/// default) is written into it.
///
/// # Safety
///
/// `param` must be a valid param handle or a null pointer; `out` must be
/// writable for the number of elements that `kind` implies; both must stay valid
/// for the duration of the call.
///
/// # Errors
///
/// `out` null -> `kOfxStatErrValue`; invalid `kind` -> `kOfxStatErrUnknown`;
/// invalid handle -> `kOfxStatErrBadHandle`; the value variant does not match
/// `kind` -> `kOfxStatFailed`.
pub unsafe extern "C" fn ofx_param_get_impl(
	param: *mut c_void,
	kind: c_int,
	out: *mut c_void,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		let k = ParamKind::from_i32(kind).ok_or(status::ERR_UNKNOWN)?;
		let r = resolve_param(param)?;
		write_value(&value_of(&r), k, out)?;
		Ok(())
	})
}

/// String get (resident pointer: points into the instance storage / the
/// definition default; valid until the next set — same contract as the property
/// suite's string pointer).
#[no_mangle]
/// String parameter read: writes the internally resident `char*` into `*out`
/// (the C shim's string-get forward target).
///
/// The pointer refers to the instance's own string storage (at describe time, to
/// the definition default) and is valid until the next set on that parameter;
/// the plugin must not free or modify it and must use it before the next set —
/// the same contract as the property suite's string pointer.
///
/// # Safety
///
/// `param` must be a valid param handle or a null pointer; `out` must be
/// writable for one `*mut c_char`. The returned string pointer is valid only
/// within the window described above.
///
/// # Errors
///
/// `out` null -> `kOfxStatErrValue`; invalid handle -> `kOfxStatErrBadHandle`;
/// the parameter is not a string type (or the instance string is missing) ->
/// `kOfxStatFailed`.
pub unsafe extern "C" fn ofx_param_get_string_impl(
	param: *mut c_void,
	out: *mut *mut c_char,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		let ptr: *const c_char = match resolve_param(param)? {
			ParamRef::Instance(p) => p.string_ptr().ok_or(status::FAILED)?,
			ParamRef::Def(d) => match &d.default {
				ParamValue::String(s) | ParamValue::StrChoice(s) => s.as_ptr(),
				_ => return Err(status::FAILED),
			},
		};
		unsafe { *out = ptr as *mut c_char };
		Ok(())
	})
}

/// Numeric set (forwarded by the C shim): `input` is a typed array for `kind`.
/// During describe (ParamDef) -> BadHandle (HS: paramSetValue's verifyMagic fails
/// for a descriptor).
#[no_mangle]
/// Numeric parameter write (the C shim's `paramSetValue*` forward target).
///
/// `input` is a C-side typed array for `kind` (mapping in [`read_value`]); after
/// a successful write to the instance it fires the `kOfxChangePluginEdited`
/// instanceChanged write-back (see [`notify_changed`]). Definition handles
/// during describe are not writable.
///
/// # Safety
///
/// `param` must be a valid param handle or a null pointer; `input` must be
/// aligned and long enough for the number of elements that `kind` implies (see
/// [`read_value`]); both must stay valid for the duration of the call.
///
/// # Errors
///
/// `input` null -> `kOfxStatErrValue`; invalid `kind` -> `kOfxStatErrUnknown`;
/// invalid handle or describe time -> `kOfxStatErrBadHandle`; a string `kind`
/// (because [`read_value`] returns `None`) -> `kOfxStatFailed`.
pub unsafe extern "C" fn ofx_param_set_impl(
	param: *mut c_void,
	kind: c_int,
	input: *const c_void,
) -> c_int {
	caught(|| {
		if input.is_null() {
			return Err(status::ERR_VALUE);
		}
		let k = ParamKind::from_i32(kind).ok_or(status::ERR_UNKNOWN)?;
		let value = read_value(k, input).ok_or(status::FAILED)?;
		let (name, props_addr) = match resolve_param(param)? {
			ParamRef::Instance(p) => {
				p.set_ofx(value);
				(p.def.name.clone(), &p.props as *const _ as usize)
			}
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		notify_changed(props_addr, &name);
		Ok(())
	})
}

/// String set.
#[no_mangle]
/// String parameter write (the C shim's string-set forward target).
///
/// Copies the NUL-terminated string `input` points to into the instance storage,
/// then fires the `kOfxChangePluginEdited` instanceChanged write-back; definition
/// handles during describe return `BadHandle` and are not written.
///
/// # Safety
///
/// `param` must be a valid param handle or a null pointer; `input` must point to
/// a valid NUL-terminated C string; both must stay valid for the duration of the
/// call.
///
/// # Errors
///
/// `input` null -> `kOfxStatErrValue`; invalid handle or describe time ->
/// `kOfxStatErrBadHandle`.
pub unsafe extern "C" fn ofx_param_set_string_impl(
	param: *mut c_void,
	input: *const c_char,
) -> c_int {
	caught(|| {
		if input.is_null() {
			return Err(status::ERR_VALUE);
		}
		let s = CString::new(unsafe { CStr::from_ptr(input) }.to_bytes()).unwrap();
		let (name, props_addr) = match resolve_param(param)? {
			ParamRef::Instance(p) => {
				p.set_ofx(ParamValue::String(s));
				(p.def.name.clone(), &p.props as *const _ as usize)
			}
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		notify_changed(props_addr, &name);
		Ok(())
	})
}

/// derivative/integral: phase 1 has no animation -> MissingHostFeature (the C
/// shim forwards without consuming va_args).
#[no_mangle]
/// Shared implementation of derivative/integral: phase 1 has no keyframe
/// animation, so it always returns `kOfxStatErrMissingHostFeature` (the C shim
/// forwards without consuming va_args).
///
/// The handle is still validated first: an empty/bad handle takes precedence and
/// returns `kOfxStatErrBadHandle` (matching HS's parameter validation order).
///
/// # Safety
///
/// `param` must be a valid param handle or a null pointer, and the handle must
/// stay valid for the duration of the call.
pub unsafe extern "C" fn ofx_param_missing_feature_impl(param: *mut c_void) -> c_int {
	caught(|| {
		// handle 合法性仍检查（空/坏句柄 → BadHandle）。
		let _ = resolve_param(param)?;
		Err(status::ERR_MISSING_HOST_FEATURE)
	})
}

// ---- 非变长入口 ----------------------------------------------------------

/// `paramDefine`: during describe, defines a parameter on the effect descriptor
/// and outputs its handle.
///
/// Accepts only a descriptor handle during describe (instance time ->
/// `BadHandle`); `name` and `param_type` are both NUL-terminated C strings. It
/// supports the numeric/string types covered by `kind_of_type`, plus the
/// value-less types Bytes, Custom, PushButton, Group, Page and Parametric
/// (parametric curve values go through the parametric suite); any other type ->
/// `kOfxStatErrUnsupported`. A duplicate name is not an error: another entry is
/// appended in insertion order (host lookup uses the first one, matching HS's
/// map-overwrite semantics).
///
/// On success `*property_set` is the property-set handle of the newly created
/// parameter (same value as the param handle); on failure it is first set to
/// NULL.
///
/// # Safety
///
/// `param_set` must be a valid param-set handle or a null pointer;
/// `param_type`/`name` must be valid NUL-terminated C strings; `property_set`
/// may be null, and when non-null must be writable for one `*mut c_void`. All
/// pointers must stay valid for the duration of the call.
///
/// # Errors
///
/// `property_set` null -> `BadHandle`; empty or non-UTF-8 name/type -> `ErrValue`;
/// invalid handle or instance time -> `BadHandle`; unsupported type ->
/// `Unsupported`.
unsafe extern "C" fn param_define(
	param_set: *mut c_void,
	param_type: *const c_char,
	name: *const c_char,
	property_set: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if property_set.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		unsafe { *property_set = std::ptr::null_mut() };
		let t = unsafe { c_name(param_type)? };
		let n = unsafe { c_name(name)? };
		// 仅 describe 期（HS: SetDescriptor 专属，实例期 dynamic_cast
		// 失败 → BadHandle）。
		let desc = match resolve_param_set(param_set)? {
			ParamSetRef::Descriptor(d) => d,
			ParamSetRef::Instance(_) => return Err(status::ERR_BAD_HANDLE),
		};
		// 类型合法性（kind_of_type 之外还有无值类与 parametric；
		// parametric 的曲线值走 OfxParametricParameterSuite）。
		let valid = crate::param::kind_of_type(t).is_some()
			|| matches!(
				t,
				crate::param::TYPE_BYTES
					| crate::param::TYPE_CUSTOM
					| crate::param::TYPE_PUSHBUTTON
					| crate::param::TYPE_GROUP
					| crate::param::TYPE_PAGE
					| crate::param::TYPE_PARAMETRIC
			);
		if !valid {
			return Err(status::ERR_UNSUPPORTED);
		}
		let def = ParamDef::new(n, t);
		desc.params.push(Box::new(def));
		// 地址必须取自已入盒的对象（栈上临时变量在 push 后失效，
		// 句柄会指向死内存——Box 负载地址在 Vec 重分配时稳定）。
		let def = desc.params.last().expect("just pushed");
		let addr = &def.props as *const _ as usize;
		unsafe {
			*property_set = tag::make(addr as *const crate::property::PropertySet, tag::PARAM_DEF)
		};
		Ok(())
	})
}

/// `paramGetHandle`: looks a parameter up by name in the param-set and outputs
/// its param handle.
///
/// During describe it searches the descriptor's parameter definitions; at
/// instance time it searches the instance's parameter table; on a match the
/// parameter property-set address plus a tag is wrapped into a handle.
/// `property_set` may be NULL; when non-null it receives the same value as
/// `param` (props at offset 0). A name that is not found returns
/// `kOfxStatErrUnknown`.
///
/// # Safety
///
/// `param_set` must be a valid param-set handle or a null pointer; `name` must be
/// a valid NUL-terminated C string; `param` must be writable for one
/// `*mut c_void`, and `property_set` likewise when non-null. All pointers must
/// stay valid for the duration of the call.
///
/// # Errors
///
/// `param` null -> `kOfxStatErrBadHandle`; empty/non-UTF-8 name -> `ErrValue`;
/// invalid handle -> `kOfxStatErrBadHandle`; name not found ->
/// `kOfxStatErrUnknown`.
unsafe extern "C" fn param_get_handle(
	param_set: *mut c_void,
	name: *const c_char,
	param: *mut *mut c_void,
	property_set: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if param.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		unsafe { *param = std::ptr::null_mut() };
		let n = unsafe { c_name(name)? };
		let handle = match resolve_param_set(param_set)? {
			ParamSetRef::Descriptor(d) => {
				let def = d.param(n).ok_or(status::ERR_UNKNOWN)?;
				let addr = &def.props as *const _ as usize;
				tag::make(addr as *const crate::property::PropertySet, tag::PARAM_DEF)
			}
			ParamSetRef::Instance(i) => {
				let p = i.params.find(n).ok_or(status::ERR_UNKNOWN)?;
				let addr = &p.props as *const _ as usize;
				tag::make(
					addr as *const crate::property::PropertySet,
					tag::PARAM_INSTANCE,
				)
			}
		};
		unsafe { *param = handle };
		if !property_set.is_null() {
			// props 在偏移 0：param handle 与 props handle 同值。
			unsafe { *property_set = handle };
		}
		Ok(())
	})
}

/// `paramSetGetPropertySet`: returns the param-set's (effect descriptor's or
/// instance's) own property-set handle.
///
/// Under the handle convention the property set and the effect handle have the
/// same value, so this only validates and writes the handle back unchanged; the
/// plugin uses it to read/write effect-level properties. Pairs with
/// [`param_get_property_set`] (the param-level property set).
///
/// # Safety
///
/// `param_set` must be a valid param-set handle or a null pointer;
/// `property_set` must be writable for one `*mut c_void`.
///
/// # Errors
///
/// `property_set` null, or `param_set` null / with a mismatched tag ->
/// `kOfxStatErrBadHandle`.
unsafe extern "C" fn param_set_get_property_set(
	param_set: *mut c_void,
	property_set: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if property_set.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let _ = resolve_param_set(param_set)?;
		unsafe { *property_set = param_set };
		Ok(())
	})
}

/// `paramGetPropertySet`: returns the property-set handle of the param handle
/// itself.
///
/// The property-set address and the param handle have the same value (the handle
/// convention with props at offset 0), so it is validated and written back
/// unchanged. The plugin uses it to read/write parameter-level properties
/// (label, display name, ...); it pairs with [`param_set_get_property_set`] (the
/// effect-level property set).
///
/// # Safety
///
/// `param` must be a valid param handle or a null pointer; `property_set` must be
/// writable for one `*mut c_void`.
///
/// # Errors
///
/// `property_set` null, or `param` null / with a mismatched tag ->
/// `kOfxStatErrBadHandle`.
unsafe extern "C" fn param_get_property_set(
	param: *mut c_void,
	property_set: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if property_set.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let _ = resolve_param(param)?;
		unsafe { *property_set = param };
		Ok(())
	})
}

/// `paramGetNumKeys`: reports the number of keyframes of a parameter.
///
/// Phase 1 has no animation, so it always writes `*count = 0`; the param handle
/// is still validated first (invalid -> `BadHandle`).
///
/// # Safety
///
/// `param` must be a valid param handle or a null pointer; `count` must be
/// writable for one `c_int`.
///
/// # Errors
///
/// `count` null -> `kOfxStatErrValue`.
unsafe extern "C" fn param_get_num_keys(param: *mut c_void, count: *mut c_int) -> c_int {
	caught(|| {
		let _ = resolve_param(param)?;
		if count.is_null() {
			return Err(status::ERR_VALUE);
		}
		unsafe { *count = 0 };
		Ok(())
	})
}

/// `paramGetKeyTime`: reads the keyframe time by key index.
///
/// Phase 1 has no keyframes, so it validates the handle and then always returns
/// `kOfxStatErrBadIndex`; `_index` is unused and `_time` is never written.
///
/// # Safety
///
/// `param` must be a valid param handle or a null pointer, and the handle must
/// stay valid for the duration of the call.
unsafe extern "C" fn param_get_key_time(
	param: *mut c_void,
	_index: c_int,
	_time: *mut c_double,
) -> c_int {
	caught(|| {
		let _ = resolve_param(param)?;
		Err(status::ERR_BAD_INDEX)
	})
}

/// `paramGetKeyIndex`: looks up a neighbouring keyframe index by time/direction.
///
/// Phase 1 has no keyframes, so it validates the handle and then always returns
/// `kOfxStatErrBadIndex`; `_time` and `_direction` are unused and `_index` is
/// never written.
///
/// # Safety
///
/// `param` must be a valid param handle or a null pointer, and the handle must
/// stay valid for the duration of the call.
unsafe extern "C" fn param_get_key_index(
	param: *mut c_void,
	_time: c_double,
	_direction: c_int,
	_index: *mut c_int,
) -> c_int {
	caught(|| {
		let _ = resolve_param(param)?;
		Err(status::ERR_BAD_INDEX)
	})
}

/// `paramDeleteKey`: deletes the keyframe at the given time.
///
/// Phase 1 has no keyframes, so after validating the handle it returns
/// `kOfxStatOK` directly (no-op); `_time` is unused (it shares the "no keyframes
/// means no-op" semantics with [`param_delete_all_keys`]).
///
/// # Safety
///
/// `param` must be a valid param handle or a null pointer, and the handle must
/// stay valid for the duration of the call.
///
/// # Errors
///
/// Empty/invalid handle -> `kOfxStatErrBadHandle`.
unsafe extern "C" fn param_delete_key(param: *mut c_void, _time: c_double) -> c_int {
	caught(|| {
		let _ = resolve_param(param)?;
		Ok(())
	})
}

/// `paramDeleteAllKeys`: deletes all keyframes of a parameter.
///
/// Phase 1 has no keyframes, so after validating the handle it returns
/// `kOfxStatOK` directly (no-op); pairs with [`param_delete_key`].
///
/// # Safety
///
/// `param` must be a valid param handle or a null pointer, and the handle must
/// stay valid for the duration of the call.
///
/// # Errors
///
/// Empty/invalid handle -> `kOfxStatErrBadHandle`.
unsafe extern "C" fn param_delete_all_keys(param: *mut c_void) -> c_int {
	caught(|| {
		let _ = resolve_param(param)?;
		Ok(())
	})
}

/// `paramCopy`: copies the keyframes of the `src` parameter to `dst`.
///
/// Phase 1 has no keyframes to copy: after validating both handles it returns
/// `kOfxStatOK` directly (no-op); `_dst_time`, `_src_time` and `_key_range` are
/// unused.
///
/// # Safety
///
/// `dst` and `src` must be valid param handles or null pointers, and both must
/// stay valid for the duration of the call (passing the same handle twice is also
/// allowed).
///
/// # Errors
///
/// Either handle null or with a mismatched tag -> `kOfxStatErrBadHandle`.
unsafe extern "C" fn param_copy(
	dst: *mut c_void,
	src: *mut c_void,
	_dst_time: c_double,
	_src_time: c_double,
	_key_range: *const c_void,
) -> c_int {
	caught(|| {
		let _ = resolve_param(dst)?;
		let _ = resolve_param(src)?;
		Ok(())
	})
}

/// `paramEditBegin`: enters an edit transaction (undo grouping).
///
/// After resolving the `param` handle it locates the owning instance through
/// `PARAM_OWNER` and increments its transaction depth; when the instance is not
/// registered this is a no-op (hand-built instances have no host registration, so
/// the edit brackets degrade to no grouping). Pairs with [`param_edit_end`];
/// write-backs inside the transaction start accumulating in
/// [`crate::instance::Instance::edit_begin`].
///
/// # Safety
///
/// `param` must be a valid param handle (definition or instance) or a null
/// pointer, and the handle must stay valid for the duration of the call.
///
/// # Errors
///
/// Empty/invalid handle or a describe-time definition handle ->
/// `kOfxStatErrBadHandle`.
unsafe extern "C" fn param_edit_begin(param: *mut c_void) -> c_int {
	caught(|| {
		let addr = match resolve_param(param)? {
			ParamRef::Instance(p) => &p.props as *const _ as usize,
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		if let Some(owner) = PARAM_OWNER
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.get(&addr)
			.copied()
		{
			let inst = unsafe { &*(owner as *const Instance) };
			inst.edit_begin();
		}
		Ok(())
	})
}

/// `paramEditEnd`: exits an edit transaction; the outermost end commits the
/// commands accumulated inside it.
///
/// After resolving the `param` handle it locates the owning instance through
/// `PARAM_OWNER` and decrements its transaction depth; at the outermost end the
/// write-backs accumulated inside the transaction are merged into one multi
/// command, redo'd into effect and released (see
/// [`crate::instance::Instance::edit_end`]). A no-op when the instance is not
/// registered. Pairs with [`param_edit_begin`].
///
/// # Safety
///
/// `param` must be a valid param handle (definition or instance) or a null
/// pointer, and the handle must stay valid for the duration of the call.
///
/// # Errors
///
/// Empty/invalid handle or a describe-time definition handle ->
/// `kOfxStatErrBadHandle`.
unsafe extern "C" fn param_edit_end(param: *mut c_void) -> c_int {
	caught(|| {
		let addr = match resolve_param(param)? {
			ParamRef::Instance(p) => &p.props as *const _ as usize,
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		if let Some(owner) = PARAM_OWNER
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.get(&addr)
			.copied()
		{
			let inst = unsafe { &*(owner as *const Instance) };
			inst.edit_end();
		}
		Ok(())
	})
}

extern "C" {
	fn ofx_param_get_value_shim(param: *mut c_void, ...) -> c_int;
	fn ofx_param_get_value_at_time_shim(param: *mut c_void, time: c_double, ...) -> c_int;
	fn ofx_param_set_value_shim(param: *mut c_void, ...) -> c_int;
	fn ofx_param_set_value_at_time_shim(param: *mut c_void, time: c_double, ...) -> c_int;
	fn ofx_param_get_derivative_shim(param: *mut c_void, time: c_double, ...) -> c_int;
	fn ofx_param_get_integral_shim(
		param: *mut c_void,
		time1: c_double,
		time2: c_double,
		...
	) -> c_int;
}

/// Static function-table instance.
pub fn suite_v1() -> &'static ParameterSuiteV1 {
	static SUITE: std::sync::OnceLock<ParameterSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| ParameterSuiteV1 {
		param_define,
		param_get_handle,
		param_set_get_property_set,
		param_get_property_set,
		param_get_value: ofx_param_get_value_shim,
		param_get_value_at_time: ofx_param_get_value_at_time_shim,
		param_get_derivative: ofx_param_get_derivative_shim,
		param_get_integral: ofx_param_get_integral_shim,
		param_set_value: ofx_param_set_value_shim,
		param_set_value_at_time: ofx_param_set_value_at_time_shim,
		param_get_num_keys,
		param_get_key_time,
		param_get_key_index,
		param_delete_key,
		param_delete_all_keys,
		param_copy,
		param_edit_begin,
		param_edit_end,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::CString;
	use std::sync::Arc;

	use crate::host::Plugin;
	use crate::param::{ParamInstance, ParamSetInstance};
	use crate::property::PropertySet;

	fn cs(s: &str) -> CString {
		CString::new(s).unwrap()
	}

	fn descriptor_handle(d: &EffectDescriptor) -> *mut c_void {
		tag::make(
			&d.props as *const crate::property::PropertySet,
			tag::DESCRIPTOR,
		)
	}

	/// Fake plugin (host::Plugin is only constructed to obtain an Arc to feed the
	/// Instance; fields other than the descriptor are not touched by this test).
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
		tag::make(
			&i.props as *const crate::property::PropertySet,
			tag::INSTANCE,
		)
	}

	/// Build an instance through paramDefine (describe output -> createInstance).
	fn make_instance() -> (Arc<Instance>, *mut c_void) {
		let desc = EffectDescriptor::new();
		let s = suite_v1();
		let dhandle = descriptor_handle(&desc);
		unsafe {
			for (t, n) in [
				("OfxParamTypeInteger", "gain"),
				("OfxParamTypeDouble", "opacity"),
				("OfxParamTypeDouble2D", "pos"),
				("OfxParamTypeRGB", "color"),
				("OfxParamTypeBoolean", "enabled"),
				("OfxParamTypeString", "label"),
			] {
				let t = cs(t);
				let n = cs(n);
				let mut ph: *mut c_void = std::ptr::null_mut();
				assert_eq!(
					(s.param_define)(dhandle, t.as_ptr(), n.as_ptr(), &mut ph),
					0
				);
			}
		}
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
		(inst.clone(), instance_handle(&inst))
	}

	#[test]
	fn describe_define_and_get() {
		let desc = EffectDescriptor::new();
		let s = suite_v1();
		let handle = descriptor_handle(&desc);
		let t = cs("OfxParamTypeDouble");
		let n = cs("opacity");

		let mut ph: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.param_define)(handle, t.as_ptr(), n.as_ptr(), &mut ph), 0);
		}
		assert!(!ph.is_null());
		assert_eq!(tag::kind(ph), tag::PARAM_DEF);

		// describe 期 paramGetValue：默认值 0.0。
		let mut v = 99.0;
		unsafe {
			assert_eq!((s.param_get_value)(ph, &mut v), 0);
		}
		assert_eq!(v, 0.0);

		// describe 期 set → BadHandle（HS: verifyMagic 对 descriptor 失败）。
		unsafe {
			assert_eq!((s.param_set_value)(ph, 42.0), status::ERR_BAD_HANDLE);
		}

		// paramGetHandle 按名取（props handle 同值）。
		let mut ph2: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.param_get_handle)(handle, n.as_ptr(), &mut ph2, std::ptr::null_mut()),
				0
			);
			assert_eq!(ph, ph2);
		}

		// 未找到 → ErrUnknown（HS:1758）。
		let nope = cs("nope");
		unsafe {
			assert_eq!(
				(s.param_get_handle)(handle, nope.as_ptr(), &mut ph2, std::ptr::null_mut()),
				status::ERR_UNKNOWN
			);
		}

		// 未定义类型 → ErrUnsupported（HS:1704）；parametric → 放行
		// （曲线读写走 OfxParametricParameterSuite）。
		let bad = cs("OfxParamTypeBogus");
		let par = cs("OfxParamTypeParametric");
		unsafe {
			assert_eq!(
				(s.param_define)(handle, bad.as_ptr(), n.as_ptr(), &mut ph2),
				status::ERR_UNSUPPORTED
			);
			assert_eq!((s.param_define)(handle, par.as_ptr(), n.as_ptr(), &mut ph2), 0);
		}
		assert_eq!(tag::kind(ph2), tag::PARAM_DEF);

		// 实例期 paramDefine → BadHandle。
		let (_inst, ih) = make_instance();
		unsafe {
			assert_eq!(
				(s.param_define)(ih, t.as_ptr(), n.as_ptr(), &mut ph2),
				status::ERR_BAD_HANDLE
			);
		}
	}

	#[test]
	fn instance_get_set_variadic_roundtrip() {
		let (_inst, ih) = make_instance();
		let s = suite_v1();

		// 取各参数 handle。
		let mut gain: *mut c_void = std::ptr::null_mut();
		let mut opacity: *mut c_void = std::ptr::null_mut();
		let mut pos: *mut c_void = std::ptr::null_mut();
		let mut color: *mut c_void = std::ptr::null_mut();
		let mut enabled: *mut c_void = std::ptr::null_mut();
		let mut label: *mut c_void = std::ptr::null_mut();
		unsafe {
			for (n, out) in [
				("gain", &mut gain),
				("opacity", &mut opacity),
				("pos", &mut pos),
				("color", &mut color),
				("enabled", &mut enabled),
				("label", &mut label),
			] {
				let n = cs(n);
				let r = (s.param_get_handle)(ih, n.as_ptr(), out, std::ptr::null_mut());
				assert_eq!(r, 0, "gethandle {} failed: {}", n.to_str().unwrap(), r);
				assert_eq!(tag::kind(*out), tag::PARAM_INSTANCE);
			}
		}

		// Integer set/get。
		unsafe {
			assert_eq!((s.param_set_value)(gain, 7), 0);
		}
		let mut v = 0;
		unsafe {
			assert_eq!((s.param_get_value)(gain, &mut v), 0);
		}
		assert_eq!(v, 7);

		// Double2D。
		unsafe {
			assert_eq!((s.param_set_value)(pos, 1.5, -2.5), 0);
		}
		let (mut x, mut y) = (0.0, 0.0);
		unsafe {
			assert_eq!((s.param_get_value)(pos, &mut x, &mut y), 0);
		}
		assert_eq!((x, y), (1.5, -2.5));

		// RGB。
		unsafe {
			assert_eq!((s.param_set_value)(color, 0.1, 0.2, 0.3), 0);
		}
		let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
		unsafe {
			assert_eq!((s.param_get_value)(color, &mut r, &mut g, &mut b), 0);
		}
		assert_eq!((r, g, b), (0.1, 0.2, 0.3));

		// Boolean（int 0/1）。
		unsafe {
			assert_eq!((s.param_set_value)(enabled, 1), 0);
		}
		let mut e = 0;
		unsafe {
			assert_eq!((s.param_get_value)(enabled, &mut e), 0);
		}
		assert_eq!(e, 1);

		// String：set 后 get 返回内驻指针。
		let hello = cs("hello");
		unsafe {
			assert_eq!((s.param_set_value)(label, hello.as_ptr()), 0);
		}
		let mut p: *mut c_char = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.param_get_value)(label, &mut p), 0);
			assert_eq!(CStr::from_ptr(p).to_bytes(), b"hello");
		}

		// AtTime == 当前值（无动画）。
		let mut v2 = 0.0;
		unsafe {
			assert_eq!((s.param_get_value_at_time)(opacity, 12.0, &mut v2), 0);
			assert_eq!(v2, 0.0);
			assert_eq!((s.param_set_value_at_time)(opacity, 12.0, 0.75), 0);
		}
		let mut v3 = 0.0;
		unsafe {
			assert_eq!((s.param_get_value)(opacity, &mut v3), 0);
		}
		assert_eq!(v3, 0.75);

		// keys：恒 0 / BadIndex。
		let mut nkeys = -1;
		unsafe {
			assert_eq!((s.param_get_num_keys)(opacity, &mut nkeys), 0);
			assert_eq!(nkeys, 0);
			let mut kt = 0.0;
			assert_eq!(
				(s.param_get_key_time)(opacity, 0, &mut kt),
				status::ERR_BAD_INDEX
			);
		}

		// derivative/integral → MissingHostFeature。
		let mut dv = 0.0;
		unsafe {
			assert_eq!(
				(s.param_get_derivative)(opacity, 1.0, &mut dv),
				status::ERR_MISSING_HOST_FEATURE
			);
		}

		// editBegin/End、deleteKey(s)、copy：no-op OK。
		unsafe {
			assert_eq!((s.param_edit_begin)(opacity), 0);
			assert_eq!((s.param_edit_end)(opacity), 0);
			assert_eq!((s.param_delete_key)(opacity, 1.0), 0);
			assert_eq!((s.param_delete_all_keys)(opacity), 0);
			assert_eq!(
				(s.param_copy)(opacity, opacity, 1.0, 1.0, std::ptr::null()),
				0
			);
		}
	}

	/// paramSetValue -> instanceChanged write-back (C++ paramChangedByPlugin):
	/// once the instance is bound to a node, a plugin-side value change locates
	/// the instance through PARAM_OWNER, notify_instance_changed produces an undo
	/// command and immediately redoes it — the node input is the new value.
	#[test]
	fn set_value_writes_back_to_bound_node() {
		let (inst, ih) = make_instance();
		let s = suite_v1();
		let mut gain: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.param_get_handle)(ih, cs("gain").as_ptr(), &mut gain, std::ptr::null_mut()),
				0
			);
		}

		// 宿主侧登记：param → 实例（生产由 createInstance 路径做），
		// 实例 → quercusnode 节点（facade 装配期做）。
		let project = quercus_node::project::Project::new();
		let node_id = {
			let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
			let id = guard.graph.add_node(
				quercus_node::node::NodeCore::empty(),
				Box::new(quercus_node::nodes::EmptyBehavior),
			);
			guard.graph.get_mut(id).unwrap().core.add_input(
				quercus_node::input::Input::new(
					"gain",
					quercus_node::value::ValueType::Int,
					quercus_node::value::NodeValue::Int(0),
				),
			);
			id
		};
		let identity = crate::node::register_node(project.clone(), node_id);
		inst.bind_node(identity as usize);
		let param_addr = tag::strip(gain) as usize;
		register_param_owner(param_addr, &inst.props as *const PropertySet as usize);

		unsafe {
			assert_eq!((s.param_set_value)(gain, 5), 0);
		}
		let stored = project
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.graph
			.get(node_id)
			.unwrap()
			.core
			.standard_value("gain", -1);
		assert_eq!(
			stored,
			quercus_node::value::NodeValue::Int(5),
			"插件改值必须经 instanceChanged 写回节点输入"
		);
		unregister_params_of(&inst.props as *const PropertySet as usize);
		crate::node::unregister_node(identity);
	}
}
