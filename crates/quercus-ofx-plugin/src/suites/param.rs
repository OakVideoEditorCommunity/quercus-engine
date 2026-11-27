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
//! - phase 2 animation: ordinary animatable parameters (`Double*`, `Integer*`,
//!   `RGB`, `RGBA`) bridge to `NodeCore::KeyframeTrack` (when bound to a node)
//!   or `ParamInstance`'s render-time/local track snapshot; derivative/integral
//!   queries continue to return `kOfxStatErrMissingHostFeature` (deferred to
//!   follow-up interpolation work); `paramEditBegin`/`paramEditEnd` group
//!   write-backs into one global undo entry.

use crate::property;
use std::collections::HashMap;
use std::env;
use std::ffi::{c_char, c_double, c_int, c_uint, c_void, CStr, CString};
use std::panic;
use std::ptr;
use std::slice;
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
	pub param_get_num_keys: unsafe extern "C" fn(*mut c_void, *mut c_uint) -> c_int,
	/// paramGetKeyTime
	pub param_get_key_time: unsafe extern "C" fn(*mut c_void, c_uint, *mut c_double) -> c_int,
	/// paramGetKeyIndex
	pub param_get_key_index:
		unsafe extern "C" fn(*mut c_void, c_double, c_int, *mut c_int) -> c_int,
	/// paramDeleteKey
	pub param_delete_key: unsafe extern "C" fn(*mut c_void, c_double) -> c_int,
	/// paramDeleteAllKeys
	pub param_delete_all_keys: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// paramCopy
	pub param_copy: unsafe extern "C" fn(
		*mut c_void,
		*mut c_void,
		c_double,
		*const crate::instance::OfxRangeD,
	) -> c_int,
	/// paramEditBegin / paramEditEnd (edit transaction brackets)
	pub param_edit_begin: unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int,
	/// paramEditEnd
	pub param_edit_end: unsafe extern "C" fn(*mut c_void) -> c_int,
}

// ---- handle resolution --------------------------------------------------

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
	// props of both object kinds are at offset 0 (handle convention).
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
	let caller = panic::Location::caller();
	let code = panic::catch_unwind(panic::AssertUnwindSafe(f)).map_or_else(
		|_| status::FAILED,
		|r| r.map_or_else(|c| c, |()| status::OK),
	);
	if env::var_os("QUERCUS_OFX_TRACE").is_some() && code != status::OK {
		eprintln!("[ofx] param suite error {code} at {caller}");
	}
	code
}

// ---- variadic parameter implementation (C shim forwarding) --------------

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
					let dst = slice::from_raw_parts_mut(out as *mut c_int, n);
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
					let dst = slice::from_raw_parts_mut(out as *mut f64, n);
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
					let dst = slice::from_raw_parts_mut(out as *mut f64, n);
					for (dd, s) in dst.iter_mut().zip(c.iter()).take(n) {
						*dd = *s;
					}
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Str | ParamKind::StrChoice => {
				// Strings go through ofx_param_get_string_impl (interior pointer).
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

/// Looks up the owning [`Instance`] for a parameter `props` address.
fn owner_instance(param_props: usize) -> Option<&'static Instance> {
	let owner = PARAM_OWNER
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&param_props)
		.copied()?;
	Some(unsafe { &*(owner as *const Instance) })
}

/// Resolves the owning [`Instance`] and bound node reference for a parameter, if bound.
fn bound_node(param_props: usize) -> Option<(&'static Instance, crate::node::NodeRef)> {
	let inst = owner_instance(param_props)?;
	let node_id = inst
		.node_identity
		.load(std::sync::atomic::Ordering::Relaxed);
	if node_id == 0 {
		return None;
	}
	let node_ref = crate::node::node_from_identity(node_id)?;
	Some((inst, node_ref))
}

/// Resolves the active keyframe track for an instance parameter (scoped
/// render-time snapshot override first, then bound node, then local track).
fn resolve_track(p: &ParamInstance) -> Option<quercus_node::keyframe::KeyframeTrack> {
	if !p.can_animate() {
		return None;
	}
	if let Some(override_track) = p.render_animation_override() {
		return override_track;
	}
	let addr = &p.props as *const _ as usize;
	if let Some((_, node_ref)) = bound_node(addr) {
		if let Ok(track) = crate::node::read_keyframe_track(&node_ref, &p.def.name) {
			return track;
		}
	}
	p.animation_track()
}

/// Evaluates a parameter at `time` (seconds).
fn eval_param_at_time(r: &ParamRef, time: f64) -> Result<ParamValue, c_int> {
	let rational = crate::node::ofx_time_to_rational(time).ok_or(status::ERR_VALUE)?;
	match r {
		ParamRef::Def(d) => Ok(d.default.clone()),
		ParamRef::Instance(p) => {
			if let Some(track) = resolve_track(p) {
				if !track.keys().is_empty() {
					if let Some(nv) = track.value_at(rational) {
						if let Some(pv) = p.value_from_node_value(&nv) {
							return Ok(pv);
						}
					}
				}
			}
			Ok(p.get())
		}
	}
}

/// instanceChanged trigger after a successful paramSetValue (HS:
/// ofxhParam.cpp:1991-1994 `paramChangedByPlugin`). A no-op when the instance
/// is not registered (the unbound-node case is already a no-op). The parametric
/// suite's Set/Add/Delete reuse this path (pub(crate): suites/parametric.rs).
pub(crate) fn notify_changed(param_props: usize, name: &str) {
	if let Some(inst) = owner_instance(param_props) {
		crate::param::notify_instance_changed(inst, name, ChangeReason::PluginEdited);
	}
}

/// Fallback invoked by the C shim when `ofx_param_kind_of` returns `0`
/// (invalid handle -> `kOfxStatErrBadHandle`, non-value/unsupported parameter
/// type -> `kOfxStatErrUnsupported`).
#[no_mangle]
pub unsafe extern "C" fn ofx_param_invalid_kind_impl(param: *mut c_void) -> c_int {
	caught(|| match resolve_param(param)? {
		ParamRef::Instance(_) | ParamRef::Def(_) => Err(status::ERR_UNSUPPORTED),
	})
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
	panic::catch_unwind(panic::AssertUnwindSafe(|| match resolve_param(param) {
		Ok(r) => match &r {
			ParamRef::Instance(p) => p.def.kind().map_or(0, |k| k as c_int),
			ParamRef::Def(d) => d.kind().map_or(0, |k| k as c_int),
		},
		Err(_) => 0,
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

/// Numeric parameter read at `time` (the C shim's `paramGetValueAtTime` target).
#[no_mangle]
pub unsafe extern "C" fn ofx_param_get_at_time_impl(
	param: *mut c_void,
	time: c_double,
	kind: c_int,
	out: *mut c_void,
) -> c_int {
	caught(|| {
		if out.is_null() || !time.is_finite() {
			return Err(status::ERR_VALUE);
		}
		let k = ParamKind::from_i32(kind).ok_or(status::ERR_UNKNOWN)?;
		let r = resolve_param(param)?;
		let val = eval_param_at_time(&r, time)?;
		write_value(&val, k, out)?;
		Ok(())
	})
}

/// String parameter read at `time` (strings are non-animatable and return
/// their current/default resident string pointer).
#[no_mangle]
pub unsafe extern "C" fn ofx_param_get_string_at_time_impl(
	param: *mut c_void,
	time: c_double,
	out: *mut *mut c_char,
) -> c_int {
	if !time.is_finite() {
		return status::ERR_VALUE;
	}
	unsafe { ofx_param_get_string_impl(param, out) }
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

/// Numeric parameter write at `time` (the C shim's `paramSetValueAtTime` target).
///
/// Writes a keyframe at `time` on animatable parameters (`can_animate()`),
/// producing an undoable node keyframe mutation when bound to a node.
#[no_mangle]
pub unsafe extern "C" fn ofx_param_set_at_time_impl(
	param: *mut c_void,
	time: c_double,
	kind: c_int,
	input: *const c_void,
) -> c_int {
	caught(|| {
		if input.is_null() || !time.is_finite() {
			return Err(status::ERR_VALUE);
		}
		let k = ParamKind::from_i32(kind).ok_or(status::ERR_UNKNOWN)?;
		let value = read_value(k, input).ok_or(status::FAILED)?;
		let p = match resolve_param(param)? {
			ParamRef::Instance(p) => p,
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		if !p.can_animate() {
			return Err(status::ERR_UNSUPPORTED);
		}
		let nv = crate::param::to_node_value(&value).ok_or(status::ERR_UNSUPPORTED)?;
		let props_addr = &p.props as *const _ as usize;
		let mut updated_node = false;
		if let Some((inst, node_ref)) = bound_node(props_addr) {
			match crate::node::upsert_keyframe_undoable(&node_ref, &p.def.name, time, &nv) {
				Ok(cmd) => {
					inst.submit_undo_command(cmd, &format!("Set {} keyframe", p.def.name));
					updated_node = true;
				}
				Err(crate::node::NodeBridgeError::UnknownInput(_)) => {}
				Err(crate::node::NodeBridgeError::InvalidTime(_)) => return Err(status::ERR_VALUE),
				Err(_) => return Err(status::FAILED),
			}
		}
		if !updated_node {
			let rational = crate::node::ofx_time_to_rational(time).ok_or(status::ERR_VALUE)?;
			let node_val = nv.to_node_value().ok_or(status::FAILED)?;
			let mut track = p.animation_track().unwrap_or_default();
			if let Some(idx) = crate::node::find_key_index_at(track.keys(), time) {
				let key_time = track.keys()[idx].time;
				track.set_key_value(key_time, node_val);
			} else {
				let interp = track
					.keys()
					.iter()
					.rfind(|k| k.time <= rational)
					.map(|k| k.interpolation)
					.unwrap_or(quercus_node::keyframe::Interpolation::Linear);
				track.set_key(quercus_node::keyframe::Keyframe {
					time: rational,
					value: node_val,
					interpolation: interp,
					bezier_in: (0.0, 0.0),
					bezier_out: (0.0, 0.0),
				});
			}
			p.set_animation_track(Some(track));
		} else {
			p.set_is_animating(true);
		}
		p.set_ofx(value);
		Ok(())
	})
}

/// String parameter write at `time`: strings are not animatable and return
/// `kOfxStatErrUnsupported` after validating handle, writability, and `time`.
#[no_mangle]
pub unsafe extern "C" fn ofx_param_set_string_at_time_impl(
	param: *mut c_void,
	time: c_double,
	input: *const c_char,
) -> c_int {
	caught(|| {
		if input.is_null() || !time.is_finite() {
			return Err(status::ERR_VALUE);
		}
		match resolve_param(param)? {
			ParamRef::Instance(_) => Err(status::ERR_UNSUPPORTED),
			ParamRef::Def(_) => Err(status::ERR_BAD_HANDLE),
		}
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
		// Handle validity is still checked (null/bad handle → BadHandle).
		let _ = resolve_param(param)?;
		Err(status::ERR_MISSING_HOST_FEATURE)
	})
}

// ---- non-variadic entry points ------------------------------------------

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
		unsafe { *property_set = ptr::null_mut() };
		let t = unsafe { c_name(param_type)? };
		let n = unsafe { c_name(name)? };
		// Describe phase only (HS: SetDescriptor-only; at instance time dynamic_cast
		// fails → BadHandle).
		let desc = match resolve_param_set(param_set)? {
			ParamSetRef::Descriptor(d) => d,
			ParamSetRef::Instance(_) => return Err(status::ERR_BAD_HANDLE),
		};
		// Type validity (beyond kind_of_type there are valueless types and parametric;
		// parametric curve values go through OfxParametricParameterSuite).
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
		// The address must be taken from the boxed object (a stack temporary is invalid after push,
		// the handle would point at dead memory — a Box payload address is stable across Vec realloc).
		let def = desc.params.last().expect("just pushed");
		let addr = &def.props as *const _ as usize;
		unsafe { *property_set = tag::make(addr as *const property::PropertySet, tag::PARAM_DEF) };
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
		unsafe { *param = ptr::null_mut() };
		let n = unsafe { c_name(name)? };
		let handle = match resolve_param_set(param_set)? {
			ParamSetRef::Descriptor(d) => {
				let def = d.param(n).ok_or(status::ERR_UNKNOWN)?;
				let addr = &def.props as *const _ as usize;
				tag::make(addr as *const property::PropertySet, tag::PARAM_DEF)
			}
			ParamSetRef::Instance(i) => {
				let p = i.params.find(n).ok_or(status::ERR_UNKNOWN)?;
				let addr = &p.props as *const _ as usize;
				tag::make(addr as *const property::PropertySet, tag::PARAM_INSTANCE)
			}
		};
		unsafe { *param = handle };
		if !property_set.is_null() {
			// props at offset 0: param handle and props handle are the same value.
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

/// Validates a parameter handle for keyframe queries and rejects non-value /
/// parametric parameter types with `kOfxStatErrUnsupported`.
fn resolve_value_param(param: *mut c_void) -> Result<ParamRef<'static>, c_int> {
	let r = resolve_param(param)?;
	let kind = match &r {
		ParamRef::Instance(p) => p.def.kind(),
		ParamRef::Def(d) => d.kind(),
	};
	if kind.is_none() {
		return Err(status::ERR_UNSUPPORTED);
	}
	Ok(r)
}

/// Resolves an edit-transaction handle (`OfxParamSetHandle` = `INSTANCE` or
/// a parameter instance handle via `PARAM_OWNER`).
fn resolve_edit_instance(handle: *mut c_void) -> Result<Option<&'static Instance>, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	match tag::kind(handle) {
		tag::INSTANCE => Ok(Some(unsafe { &*(tag::strip(handle) as *const Instance) })),
		tag::PARAM_INSTANCE => {
			let p = unsafe { &*(tag::strip(handle) as *const ParamInstance) };
			Ok(owner_instance(&p.props as *const _ as usize))
		}
		_ => Err(status::ERR_BAD_HANDLE),
	}
}

/// `paramGetNumKeys`: reports the number of keyframes of a parameter.
unsafe extern "C" fn param_get_num_keys(param: *mut c_void, count: *mut c_uint) -> c_int {
	caught(|| {
		let r = resolve_value_param(param)?;
		if count.is_null() {
			return Err(status::ERR_VALUE);
		}
		let n = match r {
			ParamRef::Instance(p) => resolve_track(p).map_or(0, |t| t.keys().len()),
			ParamRef::Def(_) => 0,
		};
		let n_uint = c_uint::try_from(n).map_err(|_| status::ERR_VALUE)?;
		unsafe { *count = n_uint };
		Ok(())
	})
}

/// `paramGetKeyTime`: reads the keyframe time by key index.
unsafe extern "C" fn param_get_key_time(
	param: *mut c_void,
	index: c_uint,
	time: *mut c_double,
) -> c_int {
	caught(|| {
		let r = resolve_value_param(param)?;
		if time.is_null() {
			return Err(status::ERR_VALUE);
		}
		let ParamRef::Instance(p) = r else {
			return Err(status::ERR_BAD_INDEX);
		};
		let track = resolve_track(p).ok_or(status::ERR_BAD_INDEX)?;
		let key = track
			.keys()
			.get(index as usize)
			.ok_or(status::ERR_BAD_INDEX)?;
		unsafe { *time = key.time.to_f64() };
		Ok(())
	})
}

/// `paramGetKeyIndex`: looks up a neighbouring keyframe index by time/direction.
///
/// Writes `*index = -1` and returns `kOfxStatFailed` when no matching key is
/// found (per `ofxParam.h`).
unsafe extern "C" fn param_get_key_index(
	param: *mut c_void,
	time: c_double,
	direction: c_int,
	index: *mut c_int,
) -> c_int {
	caught(|| {
		let r = resolve_value_param(param)?;
		if index.is_null() || !time.is_finite() {
			return Err(status::ERR_VALUE);
		}
		unsafe { *index = -1 };
		let ParamRef::Instance(p) = r else {
			return Err(status::FAILED);
		};
		let Some(track) = resolve_track(p) else {
			return Err(status::FAILED);
		};
		let keys = track.keys();
		let found = if direction == 0 {
			crate::node::find_key_index_at(keys, time)
		} else if direction > 0 {
			keys.iter()
				.position(|k| (k.time.to_f64() - time) > crate::node::KEY_TIME_EPSILON)
		} else {
			keys.iter()
				.rposition(|k| (time - k.time.to_f64()) > crate::node::KEY_TIME_EPSILON)
		};
		match found {
			Some(i) => {
				unsafe { *index = i as c_int };
				Ok(())
			}
			None => Err(status::FAILED),
		}
	})
}

/// `paramDeleteKey`: deletes the keyframe at the given time.
unsafe extern "C" fn param_delete_key(param: *mut c_void, time: c_double) -> c_int {
	caught(|| {
		let p = match resolve_value_param(param)? {
			ParamRef::Instance(p) => p,
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		if !time.is_finite() {
			return Err(status::ERR_VALUE);
		}
		if !p.can_animate() {
			return Err(status::ERR_BAD_INDEX);
		}
		let props_addr = &p.props as *const _ as usize;
		if let Some((inst, node_ref)) = bound_node(props_addr) {
			match crate::node::delete_keyframe_undoable(&node_ref, &p.def.name, time) {
				Ok(Some(cmd)) => {
					inst.submit_undo_command(cmd, &format!("Delete {} keyframe", p.def.name));
					let still_animating = resolve_track(p).is_some_and(|t| !t.keys().is_empty());
					p.set_is_animating(still_animating);
					return Ok(());
				}
				Ok(None) => return Err(status::ERR_BAD_INDEX),
				Err(crate::node::NodeBridgeError::UnknownInput(_)) => {}
				Err(crate::node::NodeBridgeError::InvalidTime(_)) => return Err(status::ERR_VALUE),
				Err(_) => return Err(status::FAILED),
			}
		}
		let mut track = p.animation_track().ok_or(status::ERR_BAD_INDEX)?;
		let idx =
			crate::node::find_key_index_at(track.keys(), time).ok_or(status::ERR_BAD_INDEX)?;
		let key_time = track.keys()[idx].time;
		if !track.remove_key(key_time) {
			return Err(status::ERR_BAD_INDEX);
		}
		p.set_animation_track(Some(track));
		Ok(())
	})
}

/// `paramDeleteAllKeys`: deletes all keyframes of a parameter.
unsafe extern "C" fn param_delete_all_keys(param: *mut c_void) -> c_int {
	caught(|| {
		let p = match resolve_value_param(param)? {
			ParamRef::Instance(p) => p,
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		if !p.can_animate() {
			return Ok(());
		}
		let props_addr = &p.props as *const _ as usize;
		if let Some((inst, node_ref)) = bound_node(props_addr) {
			match crate::node::delete_all_keyframes_undoable(&node_ref, &p.def.name) {
				Ok(Some(cmd)) => {
					inst.submit_undo_command(cmd, &format!("Clear {} keyframes", p.def.name));
					p.set_is_animating(false);
					return Ok(());
				}
				Ok(None) => {
					p.set_is_animating(false);
					return Ok(());
				}
				Err(crate::node::NodeBridgeError::UnknownInput(_)) => {}
				Err(_) => return Err(status::FAILED),
			}
		}
		p.set_animation_track(None);
		Ok(())
	})
}

/// `paramCopy`: copies the value and keyframes of `src` to `dst` with optional
/// `dst_offset` and `frame_range` filter (per `ofxParam.h`).
unsafe extern "C" fn param_copy(
	dst: *mut c_void,
	src: *mut c_void,
	dst_offset: c_double,
	frame_range: *const crate::instance::OfxRangeD,
) -> c_int {
	caught(|| {
		let dst_p = match resolve_value_param(dst)? {
			ParamRef::Instance(p) => p,
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		let src_p = match resolve_value_param(src)? {
			ParamRef::Instance(p) => p,
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		if !dst_offset.is_finite() {
			return Err(status::ERR_VALUE);
		}
		if dst_p.def.ofx_type != src_p.def.ofx_type {
			return Err(status::ERR_VALUE);
		}
		let range = if frame_range.is_null() {
			None
		} else {
			let r = unsafe { *frame_range };
			if !r.min.is_finite() || !r.max.is_finite() || r.min > r.max {
				return Err(status::ERR_VALUE);
			}
			// ofxParam.h: [0, 0] selects all animation in paramFrom.
			if r.min == 0.0 && r.max == 0.0 {
				None
			} else {
				Some(r)
			}
		};

		let src_addr = &src_p.props as *const _ as usize;
		let src_val = bound_node(src_addr)
			.and_then(|(_, node_ref)| {
				let guard = node_ref.project.lock().unwrap_or_else(|e| e.into_inner());
				let entry = guard.graph.get(node_ref.id)?;
				entry.core.input_data_type(&src_p.def.name)?;
				src_p.value_from_node_value(&entry.core.standard_value(&src_p.def.name, -1))
			})
			.unwrap_or_else(|| src_p.get());
		let src_track = resolve_track(src_p).unwrap_or_default();
		let mut new_track = quercus_node::keyframe::KeyframeTrack::default();
		if dst_p.can_animate() && !src_track.keys().is_empty() {
			for key in src_track.keys() {
				let t = key.time.to_f64();
				if let Some(r) = range {
					if t < r.min - crate::node::KEY_TIME_EPSILON
						|| t > r.max + crate::node::KEY_TIME_EPSILON
					{
						continue;
					}
				}
				let shifted = if dst_offset == 0.0 {
					key.time
				} else {
					crate::node::ofx_time_to_rational(t + dst_offset).ok_or(status::ERR_VALUE)?
				};
				let mut cloned = key.clone();
				cloned.time = shifted;
				new_track.set_key(cloned);
			}
		}

		let dst_addr = &dst_p.props as *const _ as usize;
		let mut updated_node = false;
		if let Some((inst, node_ref)) = bound_node(dst_addr) {
			if let Some(pod) = crate::param::to_node_value(&src_val) {
				if let Some(nv) = pod.to_node_value() {
					match crate::node::copy_param_undoable(
						&node_ref,
						&dst_p.def.name,
						nv,
						new_track.clone(),
					) {
						Ok(cmd) => {
							inst.submit_undo_command(cmd, &format!("Copy {}", dst_p.def.name));
							updated_node = true;
						}
						Err(crate::node::NodeBridgeError::UnknownInput(_)) => {}
						Err(_) => return Err(status::FAILED),
					}
				}
			} else if let ParamValue::String(s) | ParamValue::StrChoice(s) = &src_val {
				if let Ok(text) = s.to_str() {
					match crate::node::set_input_string_undoable(&node_ref, &dst_p.def.name, text) {
						Ok(cmd) => {
							inst.submit_undo_command(cmd, &format!("Copy {}", dst_p.def.name));
							updated_node = true;
						}
						Err(crate::node::NodeBridgeError::UnknownInput(_)) => {}
						Err(_) => return Err(status::FAILED),
					}
				}
			}
		}

		dst_p.set_ofx(src_val);
		if updated_node {
			dst_p.set_is_animating(!new_track.keys().is_empty());
		} else {
			dst_p.set_animation_track((!new_track.keys().is_empty()).then_some(new_track));
		}
		Ok(())
	})
}

/// `paramEditBegin`: enters an edit transaction (undo grouping).
///
/// Accepts either an `OfxParamSetHandle` (`INSTANCE`) per `ofxParam.h` or a
/// `PARAM_INSTANCE` handle (resolved through `PARAM_OWNER`).
unsafe extern "C" fn param_edit_begin(param_set: *mut c_void, name: *const c_char) -> c_int {
	caught(|| {
		let label = if name.is_null() {
			None
		} else {
			Some(unsafe { c_name(name)? })
		};
		if let Some(inst) = resolve_edit_instance(param_set)? {
			inst.edit_begin_with_label(label);
		}
		Ok(())
	})
}

/// `paramEditEnd`: exits an edit transaction; the outermost end commits the
/// commands accumulated inside it as one undo entry.
unsafe extern "C" fn param_edit_end(param_set: *mut c_void) -> c_int {
	caught(|| {
		if let Some(inst) = resolve_edit_instance(param_set)? {
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
mod tests;
