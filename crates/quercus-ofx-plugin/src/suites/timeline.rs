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

//! OfxTimeLineSuite v1: time queries are forwarded to the current render
//! context ([`crate::suites::RenderCtx`], frame range from the clip
//! bridge). Modeled on HS: ofxhImageEffect.cpp gTimelineSuite.
//!
//! No render context (called outside rendering) → falls back to the
//! active-viewer time source injected by the app
//! ([`set_active_viewer_provider`], the phase 6a injection point); when
//! that is absent too, the headless defaults of time 0 / time range (0,0)
//! apply. In phase 1 there is no timeline driver for gotoTime → OK no-op
//! (render time is fixed by the driver, see
//! [`crate::suites::set_render_ctx`]).

use std::ffi::{c_double, c_int, c_void};
use std::sync::{Arc, Mutex, OnceLock};

use crate::suites::{render_ctx, status};

// ---------------------------------------------------------------------------
// app 注入点：活动 viewer 时间源
// ---------------------------------------------------------------------------

/// Time information of the active viewer (the minimal set used as the
/// timeline suite's out-of-render fallback).
#[derive(Clone, Copy, Debug)]
pub struct ViewerTimeInfo {
	/// The viewer's current time (seconds).
	pub time: f64,
	/// Lower bound of the time range (seconds).
	pub range_min: f64,
	/// Upper bound of the time range (seconds).
	pub range_max: f64,
}

/// Provider of the active viewer: returning `None` means there is no
/// active viewer.
pub type ActiveViewerProvider = Arc<dyn Fn() -> Option<ViewerTimeInfo> + Send + Sync>;

static ACTIVE_VIEWER: OnceLock<Mutex<Option<ActiveViewerProvider>>> = OnceLock::new();

fn viewer_slot() -> &'static Mutex<Option<ActiveViewerProvider>> {
	ACTIVE_VIEWER.get_or_init(|| Mutex::new(None))
}

/// Registers or clears the fallback source for time queries outside
/// rendering (the app wiring point).
///
/// The timeline suite calls this provider when there is no render context
/// in order to obtain the viewer's current time and time range; passing
/// `None` unregisters it and restores the headless `0`/`(0,0)` default.
/// The registration is overwrite-style: a new provider replaces the old
/// value with no stacking.
///
/// # Arguments
///
/// * `provider` - a thread-safe closure returning `Some(ViewerTimeInfo)`
///   (`None` means there is currently no active viewer); passing `None` for
///   the whole argument clears the registered provider.
pub fn set_active_viewer_provider(provider: Option<ActiveViewerProvider>) {
	*viewer_slot().lock().unwrap_or_else(|e| e.into_inner()) = provider;
}

fn active_viewer() -> Option<ViewerTimeInfo> {
	let provider = viewer_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clone()?;
	provider()
}

/// Function table layout (OfxTimeLineSuiteV1).
#[repr(C)]
pub struct TimeLineSuiteV1 {
	/// getTime
	pub get_time: unsafe extern "C" fn(*mut c_void, *mut c_double) -> c_int,
	/// gotoTime
	pub goto_time: unsafe extern "C" fn(*mut c_void, c_double) -> c_int,
	/// getTimeBounds
	pub get_time_bounds: unsafe extern "C" fn(*mut c_void, *mut c_double, *mut c_double) -> c_int,
}

/// Shared entry template: first check whether `handle` is null (null →
/// `ERR_BAD_HANDLE`), then run `f` inside `catch_unwind`, converting a
/// panic raised inside the callback into `FAILED` so unwinding never
/// crosses the `extern "C"` boundary.
///
/// # Arguments
///
/// * `handle` - host handle; only checked for null, never dereferenced.
/// * `f` - the actual suite callback body, returning an OFX status code.
///
/// # Safety
///
/// `handle` is only used for the null check, so any value is safe; overall
/// safety depends on how `f` interprets its own pointer arguments.
unsafe fn caught(handle: *mut c_void, f: impl FnOnce() -> c_int) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		if handle.is_null() {
			return status::ERR_BAD_HANDLE;
		}
		f()
	}))
	.unwrap_or(status::FAILED)
}

/// OfxTimeLineSuiteV1 `getTime`: writes the current render time into
/// `time`.
///
/// Lookup order: the render context's `time` → the time of the active
/// viewer injected by the app → `0.0` (headless default).
///
/// # Arguments
///
/// * `handle` - instance handle passed by the host; only used for the null
///   check (null → `ERR_BAD_HANDLE`).
/// * `time` - output: current time in seconds; a null pointer returns
///   `ERR_VALUE`.
///
/// # Returns
///
/// An OFX status code: `OK`, `ERR_BAD_HANDLE` or `ERR_VALUE`; `FAILED`
/// when a panic occurs inside the callback.
///
/// # Safety
///
/// `time` must be a valid, writable `c_double` pointer (or null) whose
/// lifetime covers this call; `handle` may be null and is never
/// dereferenced.
unsafe extern "C" fn timeline_get_time(handle: *mut c_void, time: *mut c_double) -> c_int {
	unsafe {
		caught(handle, || {
			if time.is_null() {
				return status::ERR_VALUE;
			}
			// 渲染上下文 → 活动 viewer → 0（headless 默认）。
			*time = render_ctx()
				.map(|c| c.time)
				.or_else(|| active_viewer().map(|v| v.time))
				.unwrap_or(0.0);
			status::OK
		})
	}
}

/// OfxTimeLineSuiteV1 `gotoTime`: in phase 1 the host does not move the
/// timeline for plugins (render time is fixed by the driver), so after
/// checking the handle it returns `OK` directly without changing any
/// state.
///
/// # Arguments
///
/// * `handle` - instance handle passed by the host; only used for the null
///   check (null → `ERR_BAD_HANDLE`).
/// * `_time` - time in seconds the plugin asks to jump to; currently
///   ignored.
///
/// # Returns
///
/// `OK`, or `ERR_BAD_HANDLE` when the handle is null; `FAILED` when a
/// panic occurs inside the callback.
///
/// # Safety
///
/// `handle` is only checked for null and never dereferenced, so any opaque
/// pointer value may be passed (including null).
unsafe extern "C" fn timeline_goto_time(handle: *mut c_void, _time: c_double) -> c_int {
	unsafe { caught(handle, || status::OK) }
}

/// OfxTimeLineSuiteV1 `getTimeBounds`: writes the current time range into
/// `min`/`max`.
///
/// Lookup order: the render context's frame range → the range of the
/// active viewer injected by the app → `(0.0, 0.0)` (headless default).
///
/// # Arguments
///
/// * `handle` - instance handle passed by the host; only used for the null
///   check (null → `ERR_BAD_HANDLE`).
/// * `min` - output: lower bound of the time range, seconds.
/// * `max` - output: upper bound of the time range, seconds.
///
/// # Returns
///
/// An OFX status code: `OK`, or `ERR_BAD_HANDLE`/`ERR_VALUE`; `FAILED`
/// when a panic occurs inside the callback.
///
/// # Safety
///
/// `min` and `max` must be valid, writable, non-overlapping `c_double`
/// pointers (or null, in which case `ERR_VALUE` is returned), and their
/// lifetimes must cover this call.
unsafe extern "C" fn timeline_get_time_bounds(
	handle: *mut c_void,
	min: *mut c_double,
	max: *mut c_double,
) -> c_int {
	unsafe {
		caught(handle, || {
			if min.is_null() || max.is_null() {
				return status::ERR_VALUE;
			}
			// 渲染上下文 → 活动 viewer → (0,0)（headless 默认）。
			let r = render_ctx()
				.map(|c| (c.range.min, c.range.max))
				.or_else(|| active_viewer().map(|v| (v.range_min, v.range_max)))
				.unwrap_or((0.0, 0.0));
			*min = r.0;
			*max = r.1;
			status::OK
		})
	}
}

/// Static suite instance.
pub fn suite_v1() -> &'static TimeLineSuiteV1 {
	static SUITE: std::sync::OnceLock<TimeLineSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| TimeLineSuiteV1 {
		get_time: timeline_get_time,
		goto_time: timeline_goto_time,
		get_time_bounds: timeline_get_time_bounds,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::instance::{OfxRangeD, RenderScale};
	use crate::suites::{set_render_ctx, RenderCtx};

	#[test]
	fn queries_forward_to_render_ctx() {
		let s = suite_v1();
		let handle = 0x10usize as *mut c_void;
		let mut t = 0.0;
		let mut min = 0.0;
		let mut max = 0.0;

		// 无上下文：0 / (0,0)。
		unsafe {
			assert_eq!((s.get_time)(handle, &mut t), status::OK);
			assert_eq!(t, 0.0);
			assert_eq!((s.get_time_bounds)(handle, &mut min, &mut max), status::OK);
			assert_eq!((min, max), (0.0, 0.0));
		}

		// 有上下文：转发。
		set_render_ctx(Some(RenderCtx {
			time: 42.5,
			scale: RenderScale { x: 1.0, y: 1.0 },
			range: OfxRangeD {
				min: 10.0,
				max: 200.0,
			},
		}));
		unsafe {
			assert_eq!((s.get_time)(handle, &mut t), status::OK);
			assert_eq!(t, 42.5);
			assert_eq!((s.get_time_bounds)(handle, &mut min, &mut max), status::OK);
			assert_eq!((min, max), (10.0, 200.0));
			assert_eq!((s.goto_time)(handle, 99.0), status::OK);
		}
		set_render_ctx(None);

		// 空 handle / 空 out。
		unsafe {
			assert_eq!(
				(s.get_time)(std::ptr::null_mut(), &mut t),
				status::ERR_BAD_HANDLE
			);
			assert_eq!(
				(s.get_time)(handle, std::ptr::null_mut()),
				status::ERR_VALUE
			);
		}
	}
}
