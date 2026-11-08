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

//! Refcounted-handle scaffolding for the quercusengine facade boundary.
//!
//! Single-lib unification made module-to-module calls plain Rust; the
//! facade (quercusengine) is the only remaining consumer of `CHandle`s in
//! this crate — it boxes quercusnode domain objects (`Project`,
//! `NodeRef`) and small ABI payloads behind [`CHandle`]s so the frozen
//! C API keeps working unchanged, and quercusstorage reuses the same boxes
//! for the write-through session. This module is that surface:
//! [`make_owned`]/[`make_owned_with`] create the boxes, [`get`] borrows
//! their payloads, [`RefBox`] is the box layout.
//!
//! The crate's own object references never travel through handles, and
//! the panic-catching `guard*` wrappers from the old FFI era were
//! removed together with the crate's C exports (quercusengine has its own
//! guard layer).

use std::any::Any;
use std::sync::atomic::{AtomicU32, Ordering};

/// ABI version stamped into every handle.
pub const QUERCUSNODE_ABI_VERSION: u32 = 1;

/// Heap box behind a handle's `ctx`.
///
/// `repr(C)`: the field order is the stable prefix layout
/// [`RefBoxHeader`] relies on for type discrimination across boxes of
/// different payloads sharing one handle channel (the render seam
/// downcasts plugin-job boxes vs texture boxes).
#[repr(C)]
pub struct RefBox<T: ?Sized> {
	/// Atomic reference count.
	pub refs: AtomicU32,
	/// Type identity of the boxed value (stamped by [`make_owned`];
	/// [`get_checked`] compares it before reading [`RefBox::value`]).
	pub type_id: std::any::TypeId,
	/// Boxed value.
	pub value: T,
}

/// The fixed-size prefix of every [`RefBox`] (layout-stable across
/// payload types because [`RefBox`] is `repr(C)`).
#[repr(C)]
struct RefBoxHeader {
	/// Mirror of [`RefBox::refs`] (present for the layout prefix;
	/// never read here).
	_refs: AtomicU32,
	type_id: std::any::TypeId,
}

/// The shared ABI value-handle type (single-lib unification, see
/// `docs/zh/plans/riir/single-lib.md`): one canonical
/// `{ctx, addref, release, abi_version}` type in `quercuscore-rs`, re-exported
/// here so the facade's handle scaffolding stays source-compatible.
pub use quercus_core::handle::CHandle;

/// Increment a box's reference count by one.
///
/// Installed as the `addref` routine of every [`CHandle`] created by
/// [`make_owned`]/[`make_owned_with`]. Relaxed ordering suffices because the
/// count protects the box's lifetime, not the payload's contents. A borrowed
/// copy obtained this way only extends the box's lifetime, never the borrowed
/// object's.
///
/// # Safety
///
/// `ctx` must be a non-null pointer to a live `RefBox<T>` whose count has not
/// reached zero yet. Once the last release has run, the pointer dangles and
/// must not be passed here.
unsafe extern "C" fn refbox_addref<T: Any + Send>(ctx: *mut std::ffi::c_void) {
	unsafe {
		let rb = ctx as *const RefBox<T>;
		// Caller guarantees the handle is alive (ctx non-null and not
		// released) for the duration of the call.
		(*rb).refs.fetch_add(1, Ordering::Relaxed);
	}
}

/// Drop one reference to a box, destroying it when the count reaches zero.
///
/// This is the `release` routine that [`make_owned`] installs. The decrement
/// uses `AcqRel` so the thread that observes zero sees every write made through
/// earlier references (including state the payload's destructor needs); at zero
/// the box and its payload are freed. Release is not idempotent and does not
/// return whether the box was destroyed.
///
/// # Safety
///
/// `ctx` must be a non-null pointer to a live `RefBox<T>` created by
/// [`make_owned`] with a count of at least 1. The same reference must not be
/// released twice, and no other reference may be used after the call that drops
/// the count to zero — that call invalidates `ctx`.
unsafe extern "C" fn refbox_release_owned<T: Any + Send>(ctx: *mut std::ffi::c_void) {
	unsafe {
		let rb = ctx as *mut RefBox<T>;
		// AcqRel: the zeroing side must observe every write from the last
		// reference (including state the destructor needs).
		if (*rb).refs.fetch_sub(1, Ordering::AcqRel) == 1 {
			drop(Box::from_raw(rb));
		}
	}
}

/// Box `value` behind a new owned [`CHandle`] with a reference count of 1.
///
/// `T` must be `Any + Send`: the box records `TypeId` so `get_checked` can
/// reject handles that box a different payload type (the render seam probes
/// texture channels for plugin-job payloads that way), and `Send` because
/// handles cross threads through the C ABI. The handle carries
/// [`QUERCUSNODE_ABI_VERSION`] and the box's own addref/release routines.
///
/// # Returns
///
/// A non-null handle; allocation failure aborts the process rather than
/// returning an empty handle. The caller owns the single reference and must call
/// the `release` routine exactly once, using `get`/`get_checked` to borrow the
/// payload while the reference is alive.
pub fn make_owned<T: Any + Send>(value: T) -> CHandle {
	let rb = Box::into_raw(Box::new(RefBox {
		refs: AtomicU32::new(1),
		type_id: std::any::TypeId::of::<T>(),
		value,
	}));
	CHandle {
		ctx: rb as *mut std::ffi::c_void,
		addref: Some(refbox_addref::<T>),
		release: Some(refbox_release_owned::<T>),
		abi_version: QUERCUSNODE_ABI_VERSION,
	}
}

/// Box `value` behind a new owned [`CHandle`] whose `release` routine is
/// supplied by the caller.
///
/// Everything except `release` matches [`make_owned`]: count 1, type-stamped
/// payload, [`QUERCUSNODE_ABI_VERSION`], and the same `addref`. It exists for
/// boxes that need extra bookkeeping when the last reference goes away (the
/// facade's alive-counted project boxes update a debug counter in their release
/// routine), so the supplied function must perform the refcount decrement and
/// destruction itself.
///
/// # Arguments
///
/// * `value` - Payload to box; ownership moves into the handle.
/// * `release` - Routine the owner calls to drop one reference; it receives the
///   box pointer from `CHandle::ctx`.
///
/// # Safety
///
/// `release` must interpret `ctx` as a live `RefBox<T>` for this call's `T`,
/// decrement `refs` (for example `fetch_sub` with `AcqRel`), drop the box
/// exactly when the count reaches zero, and be callable once per acquired
/// reference from any thread. It must not free early, run twice for the same
/// reference, or assume the payload has no destructor work left to do.
pub fn make_owned_with<T: Any + Send>(
	value: T,
	release: unsafe extern "C" fn(*mut std::ffi::c_void),
) -> CHandle {
	let rb = Box::into_raw(Box::new(RefBox {
		refs: AtomicU32::new(1),
		type_id: std::any::TypeId::of::<T>(),
		value,
	}));
	CHandle {
		ctx: rb as *mut std::ffi::c_void,
		addref: Some(refbox_addref::<T>),
		release: Some(release),
		abi_version: QUERCUSNODE_ABI_VERSION,
	}
}

/// Typed view into a handle; `None` for empty handles.
///
/// # Safety
/// `T` must be the boxed type.
pub unsafe fn get<T: Any>(h: &CHandle) -> Option<&T> {
	if h.ctx.is_null() {
		return None;
	}
	unsafe { Some(&(*(h.ctx as *const RefBox<T>)).value) }
}

/// Typed view with type discrimination: `None` when the handle is
/// empty **or** boxes a different payload type (the render seam probes
/// texture-channel handles for plugin-job payloads this way without
/// knowing the producer).
///
/// # Safety
/// `h` must be either empty or a live handle created by
/// [`make_owned`]/[`make_owned_with`] for the duration of the call.
pub unsafe fn get_checked<T: Any>(h: &CHandle) -> Option<&T> {
	if h.ctx.is_null() {
		return None;
	}
	// SAFETY: every live box starts with the repr(C) RefBox prefix;
	// the caller guarantees the handle is alive.
	let header = unsafe { &*(h.ctx as *const RefBoxHeader) };
	if header.type_id != std::any::TypeId::of::<T>() {
		return None;
	}
	unsafe { Some(&(*(h.ctx as *const RefBox<T>)).value) }
}
