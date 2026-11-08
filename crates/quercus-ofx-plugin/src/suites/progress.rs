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

//! OfxProgressSuite v1/v2: progress → the current render's
//! [`crate::progress::ProgressReporter`].
//!
//! Location: TLS (the render driver calls [`set_current`] before invoking a
//! plugin action and clears it after the action returns) — progress is only
//! meaningful during rendering, so there is no global state (progress.rs docs).
//! Semantics: update returns false (cancelled) → kOfxStatReplyNo
//! (HS: ofxhImageEffect.cpp gProgressSuite); no reporter (called outside
//! rendering) → silent OK. start/end are the brackets, OK.

use std::ffi::{c_char, c_double, c_int, c_void};

use crate::progress::ProgressReporter;
use crate::suites::status;

thread_local! {
	static CURRENT: std::cell::RefCell<Option<ProgressReporter>> = const { std::cell::RefCell::new(None) };
}

/// Sets or clears the current thread's progress reporter (`None` = clear).
///
/// The render driver installs it before invoking a plugin action and clears it
/// afterwards; every progress-suite entry point reads it through thread-local
/// storage (TLS), so there is no global state and parallel render threads do not
/// affect each other. It is public because the render driver lives at the host
/// layer and tests inject it directly. Ownership of `reporter` moves into TLS;
/// replacing or clearing it drops the old value without notifying the UI.
pub fn set_current(reporter: Option<ProgressReporter>) {
	CURRENT.with(|c| *c.borrow_mut() = reporter);
}

/// Reads the current reporter's cancellation state (used for the image effect suite's abort).
pub(crate) fn is_cancelled() -> bool {
	CURRENT.with(|c| c.borrow().as_ref().is_some_and(|r| r.is_cancelled()))
}

/// Forwards one progress value to the current TLS reporter and converts it to an OFX status code.
///
/// `progress` is the SDK's 0.0–1.0 completion fraction; this layer does not clamp
/// it, and out-of-range values are passed to the reporter as-is. With a reporter,
/// its `update` return value decides cancellation; without one (a call outside
/// rendering) it counts as not cancelled.
///
/// # Returns
///
/// Cancelled → kOfxStatReplyNo (the plugin then aborts), otherwise kOfxStatOK.
fn progress_update(progress: f64) -> c_int {
	let cancelled = CURRENT.with(|c| match c.borrow().as_ref() {
		Some(r) => !r.update(progress),
		None => false,
	});
	if cancelled {
		status::REPLY_NO
	} else {
		status::OK
	}
}

/// Common entry-point template: runs `f` inside `catch_unwind`, turning a panic
/// into kOfxStatFailed.
///
/// Plugins are C ABI callers and a panic crossing an `extern "C"` boundary is
/// undefined behavior, so every progress-suite entry point goes through here; the
/// OFX status code `f` returns is passed through unchanged.
fn caught(f: impl FnOnce() -> c_int) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(status::FAILED)
}

/// Function-table layout (OfxProgressSuiteV1).
#[repr(C)]
pub struct ProgressSuiteV1 {
	/// progressStart
	pub start: unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int,
	/// progressUpdate (a non-kOfxStatOK return means cancelled)
	pub update: unsafe extern "C" fn(*mut c_void, c_double) -> c_int,
	/// progressEnd
	pub end: unsafe extern "C" fn(*mut c_void) -> c_int,
}

/// Function-table layout (OfxProgressSuiteV2: start takes label + message).
#[repr(C)]
pub struct ProgressSuiteV2 {
	/// progressStart
	pub start: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
	/// progressUpdate
	pub update: unsafe extern "C" fn(*mut c_void, c_double) -> c_int,
	/// progressEnd
	pub end: unsafe extern "C" fn(*mut c_void) -> c_int,
}

/// Decodes a C string into `&str`, for optional text such as label/message.
///
/// A null pointer and invalid UTF-8 both degrade to an empty string, so callers
/// need no null check. The returned borrow lives as long as the storage `p` points
/// to, so callers must use it before the original C string goes away (this file
/// copies it immediately with `to_string()`).
///
/// # Safety
///
/// `p` must be a valid NUL-terminated C string when non-null, and must stay valid
/// for the lifetime of the returned borrow (the pointer usually comes from the
/// plugin's FFI call arguments).
unsafe fn decode<'a>(p: *const c_char) -> &'a str {
	if p.is_null() {
		return "";
	}
	unsafe { std::ffi::CStr::from_ptr(p) }
		.to_str()
		.unwrap_or("")
}

/// progressStart (v1): opens the progress bracket, carrying only `label`.
///
/// With a TLS reporter it calls [`ProgressReporter::install_ui`] (`label` as the
/// title, message as an empty string) and the app's factory builds the progress UI;
/// without a reporter, with an existing C callback, or with the UI already
/// installed (nested brackets) it is a silent no-op. `label` is decoded up to the
/// first NUL; NULL/invalid UTF-8 count as an empty string. Always returns
/// kOfxStatOK (panic fallback → kOfxStatFailed). Version 2,
/// [`progress_start_v2`], adds a message.
///
/// # Safety
///
/// `_handle` is ignored and may be NULL (TLS lookup); `label` must be a valid
/// NUL-terminated C string when non-null.
unsafe extern "C" fn progress_start_v1(_handle: *mut c_void, label: *const c_char) -> c_int {
	caught(|| {
		let label = unsafe { decode(label) }.to_string();
		CURRENT.with(|c| {
			if let Some(r) = c.borrow().as_ref() {
				r.install_ui(&label, "");
			}
		});
		status::OK
	})
}

/// progressUpdate (v1): forwards `progress` to the current reporter and returns
/// whether the plugin should abort.
///
/// Shares [`progress_update`] with v2: reporter says cancelled → kOfxStatReplyNo;
/// no reporter (outside rendering) or not cancelled → kOfxStatOK; panic fallback →
/// kOfxStatFailed. `progress` is a 0.0–1.0 completion fraction; this layer does no
/// range validation.
///
/// # Safety
///
/// `_handle` is ignored and may be NULL (TLS lookup); `progress` is a plain number,
/// so there are no other preconditions.
unsafe extern "C" fn progress_update_v1(_handle: *mut c_void, progress: c_double) -> c_int {
	caught(|| progress_update(progress))
}

/// progressEnd (v1): closes the progress bracket, calling the reporter's
/// [`ProgressReporter::end_ui`] to retract the progress UI.
///
/// It is a silent no-op without a TLS reporter (a call outside rendering); always
/// returns kOfxStatOK (panic fallback → kOfxStatFailed). Paired with
/// [`progress_start_v1`] and called in pairs by the render driver.
///
/// # Safety
///
/// `_handle` is ignored and may be NULL; there are no other preconditions.
unsafe extern "C" fn progress_end_v1(_handle: *mut c_void) -> c_int {
	caught(|| {
		CURRENT.with(|c| {
			if let Some(r) = c.borrow().as_ref() {
				r.end_ui();
			}
		});
		status::OK
	})
}

/// progressStart (v2): opens the progress bracket, carrying two optional texts,
/// `label` (title) and `message` (subtext), handed to the app's UI factory.
///
/// With a TLS reporter it calls [`ProgressReporter::install_ui`] and the factory
/// builds the progress UI; without a reporter, with an existing C callback, or with
/// the UI already installed (nested brackets) it is a silent no-op. Both strings are
/// decoded up to the first NUL; NULL/invalid UTF-8 count as an empty string. Always
/// returns kOfxStatOK (panic fallback → kOfxStatFailed): the SDK contract requires
/// that start cannot be interrupted by cancellation.
///
/// # Safety
///
/// `_handle` is ignored and may be NULL (TLS lookup); `label` and `message` must be
/// valid NUL-terminated C strings when non-null.
unsafe extern "C" fn progress_start_v2(
	_handle: *mut c_void,
	label: *const c_char,
	message: *const c_char,
) -> c_int {
	caught(|| {
		let label = unsafe { decode(label) }.to_string();
		let message = unsafe { decode(message) }.to_string();
		CURRENT.with(|c| {
			if let Some(r) = c.borrow().as_ref() {
				r.install_ui(&label, &message);
			}
		});
		status::OK
	})
}

/// progressUpdate (v2): forwards `progress` to the current reporter and returns
/// whether the plugin should abort.
///
/// Fully shares [`progress_update`] with v1: reporter says cancelled →
/// kOfxStatReplyNo; no reporter (outside rendering) or not cancelled → kOfxStatOK;
/// panic fallback → kOfxStatFailed. `progress` is a 0.0–1.0 completion fraction;
/// this layer does no range validation.
///
/// # Safety
///
/// `_handle` is the plugin's progress handle, ignored by this implementation (TLS
/// lookup), and may be NULL; `progress` is a plain number, so there are no other
/// preconditions.
unsafe extern "C" fn progress_update_v2(_handle: *mut c_void, progress: c_double) -> c_int {
	caught(|| progress_update(progress))
}

/// progressEnd (v2): closes the progress bracket and always returns kOfxStatOK.
///
/// Unlike v1, v2's end does not call [`ProgressReporter::end_ui`]; this
/// implementation only provides the panic fallback (failure → kOfxStatFailed) and
/// does not touch the UI.
///
/// # Safety
///
/// `_handle` is the plugin's progress handle, ignored by this implementation (TLS
/// lookup), and may be NULL; there are no other preconditions.
unsafe extern "C" fn progress_end_v2(_handle: *mut c_void) -> c_int {
	caught(|| status::OK)
}

/// Static function-table instance (v1).
pub fn suite_v1() -> &'static ProgressSuiteV1 {
	static SUITE: std::sync::OnceLock<ProgressSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| ProgressSuiteV1 {
		start: progress_start_v1,
		update: progress_update_v1,
		end: progress_end_v1,
	})
}

/// Static function-table instance (v2).
pub fn suite_v2() -> &'static ProgressSuiteV2 {
	static SUITE: std::sync::OnceLock<ProgressSuiteV2> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| ProgressSuiteV2 {
		start: progress_start_v2,
		update: progress_update_v2,
		end: progress_end_v2,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Cancelling exit (header contract: non-zero = abort).
	unsafe extern "C" fn cancel_cb(_p: f64, _userdata: *mut c_void) -> c_int {
		1
	}

	/// Silent: update always OK; with a cancelling callback: update → REPLY_NO and
	/// the abort query is true (sticky).
	#[test]
	fn update_cancel_and_abort() {
		// 无报告器（渲染外）：静默 OK。
		set_current(None);
		let s = suite_v1();
		unsafe {
			assert_eq!((s.update)(std::ptr::null_mut(), 0.5), status::OK);
			assert_eq!(
				(s.start)(std::ptr::null_mut(), std::ptr::null()),
				status::OK
			);
			assert_eq!((s.end)(std::ptr::null_mut()), status::OK);
		}

		// 带取消回调：update → REPLY_NO；is_cancelled 为真。
		set_current(Some(unsafe {
			ProgressReporter::new(cancel_cb, std::ptr::null_mut())
		}));
		unsafe {
			assert_eq!((s.update)(std::ptr::null_mut(), 0.5), status::REPLY_NO);
		}
		assert!(is_cancelled());
		set_current(None);
		assert!(!is_cancelled());
	}
}
