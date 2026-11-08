//! Session-local edit subscriptions with deterministic callback-data cleanup.

use std::ffi::c_void;
use std::sync::Arc;

use crate::editor::EditorSessionHandle;

/// Native edit callback: operation code and the revision after command execution.
pub(crate) type EditCallback = unsafe extern "C" fn(*mut c_void, u32, i64);
/// Optional destructor for callback-owned user data.
pub(crate) type DropCallback = unsafe extern "C" fn(*mut c_void);

/// Shared registration cloned out of the session lock before notification.
pub(crate) struct SessionObserver {
	/// Stable ID used for idempotent unsubscribe and Drop cleanup.
	pub(crate) id: u64,
	callback: EditCallback,
	user_data: *mut c_void,
	drop_callback: Option<DropCallback>,
}

// Callback data is foreign-owned. The ABI requires it to be valid for calls and
// safe to release on any thread after the final Arc reference disappears.
unsafe impl Send for SessionObserver {}
unsafe impl Sync for SessionObserver {}

impl Drop for SessionObserver {
	fn drop(&mut self) {
		if let Some(drop_callback) = self.drop_callback {
			unsafe { drop_callback(self.user_data) };
		}
	}
}

/// Registers one native edit callback on the session and returns its nonzero
/// subscription ID.
///
/// The callback fires after each executed edit command as
/// `callback(user_data, op, revision)`, where `op` is the operation code and
/// `revision` is the undo-stack revision after the command. Registrations are
/// session-local, kept in insertion order, and notified from a snapshot taken
/// outside the session lock. IDs start at 1 and wrap past 0, so a returned ID is
/// always valid for [`quercus_editor_edit_unsubscribe`].
///
/// # Arguments
///
/// * `session` - Editor session handle; null returns 0.
/// * `callback` - The callback to invoke; a null callback returns 0.
/// * `user_data` - Opaque pointer passed back to every callback; the
///   registration takes ownership of it from now on.
/// * `drop_callback` - Optional destructor for `user_data`, called exactly once
///   when the registration is dropped (after any in-flight notification has
///   released its reference).
///
/// # Safety
///
/// `session` must be a valid handle or null; `callback` and `drop_callback` must
/// stay valid functions and `user_data` must stay valid for as long as the
/// registration lives. Callbacks run on whichever thread executes an edit, so
/// the data must be thread-safe (the registration is `Send`/`Sync` by contract
/// with the caller). Do not free `user_data` after subscription: the drop
/// callback owns that step.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_edit_subscribe(
	session: *mut EditorSessionHandle,
	callback: Option<EditCallback>,
	user_data: *mut c_void,
	drop_callback: Option<DropCallback>,
) -> u64 {
	if session.is_null() {
		return 0;
	}
	let Some(callback) = callback else {
		return 0;
	};
	let mut state = unsafe { &*session }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let id = state.next_observer_id;
	state.next_observer_id = state.next_observer_id.wrapping_add(1).max(1);
	state.observers.push(Arc::new(SessionObserver {
		id,
		callback,
		user_data,
		drop_callback,
	}));
	id
}

/// Removes one subscription from the session.
///
/// Returns `false` when `session` is null, `id` is 0, or no registration has
/// that id (in which case nothing is removed). The observer stops being
/// notified immediately; its `user_data` is destroyed by the registration's
/// `drop_callback` only after any notification that already cloned the observer
/// finishes, so the caller must not free the data itself.
///
/// # Safety
///
/// `session` must be a valid handle or null. The caller must not use the
/// subscription's `user_data` after this returns; it may still be live in an
/// in-flight notification until the drop callback runs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_edit_unsubscribe(
	session: *mut EditorSessionHandle,
	id: u64,
) -> bool {
	if session.is_null() || id == 0 {
		return false;
	}
	let mut state = unsafe { &*session }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let before = state.observers.len();
	state.observers.retain(|observer| observer.id != id);
	before != state.observers.len()
}

/// Calls a snapshot of current subscribers outside the session mutex. Panics in
/// foreign callbacks are contained so they cannot cross the C ABI or abort edit.
pub(crate) fn notify(session: &EditorSessionHandle, op: u32) {
	let (observers, revision) = {
		let state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
		(state.observers.clone(), quercus_undo::global::index().unwrap_or(0))
	};
	for observer in observers {
		let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
			(observer.callback)(observer.user_data, op, revision);
		}));
	}
}
