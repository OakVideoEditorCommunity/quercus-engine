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

//! Progress reporting (the host side of the Progress suite).
//!
//! Corresponds to C++'s `PluginProgressReporter`. Progress/cancellation
//! leaves the crate through a callback registered by the facade (M9's
//! facade-callback pattern). Cancellation is sticky: once the callback
//! answers false, this reporter's [`is_cancelled`] (the image effect suite's
//! abort query) stays true.
//!
//! ## app injection point (stage 6a)
//!
//! In addition to the facade C callback, the app can register a factory
//! through [`set_reporter_factory`]: when no C callback is installed for a
//! render, the Progress suite's progressStart creates a [`UiProgressReporter`]
//! from the factory on the spot (e.g. a progress dialog), progressUpdate
//! forwards to it, and a false return means cancel. Corresponds to the
//! creation path of C++'s `PluginProgressDialogReporter`.

use std::ffi::c_int;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Progress UI reporter (app-implemented: dialog, status bar, etc.). `update`
/// returning false means the user requested cancellation (mapped to
/// kOfxStatReplyNo).
pub trait UiProgressReporter: Send {
	/// Report progress (0.0..=1.0); false = cancel.
	fn update(&mut self, progress: f64) -> bool;
	/// progressEnd notification (default no-op; the app can close the progress
	/// UI / forward the completion event over IPC — the worker side's progress
	/// reporting relies on it).
	fn end(&mut self) {}
}

/// Reporter factory: called by progressStart with (label, message) to create
/// a [`UiProgressReporter`] on the spot.
pub type ReporterFactory =
	Arc<dyn Fn(&str, &str) -> Box<dyn UiProgressReporter> + Send + Sync>;

static REPORTER_FACTORY: OnceLock<Mutex<Option<ReporterFactory>>> = OnceLock::new();

fn factory_slot() -> &'static Mutex<Option<ReporterFactory>> {
	REPORTER_FACTORY.get_or_init(|| Mutex::new(None))
}

/// Register/clear the progress UI factory (the app's wiring point;
/// overwrite semantics).
///
/// `Some(factory)` replaces the current factory (the old factory is dropped;
/// reporters already created are unaffected), `None` clears the registration.
/// There is a single global slot and the last registration wins; if the lock
/// is poisoned, the inner state is recovered and execution continues without
/// panicking.
pub fn set_reporter_factory(factory: Option<ReporterFactory>) {
	*factory_slot().lock().unwrap_or_else(|e| e.into_inner()) = factory;
}

/// Get the currently registered UI factory (`None` when absent).
///
/// Returns a clone of the `Arc` rather than a reference holding the lock: the
/// caller creates a reporter outside the lock and does not block
/// [`set_reporter_factory`]. progressStart uses it to install the UI reporter
/// (see [`ProgressReporter::install_ui`]).
pub(crate) fn reporter_factory() -> Option<ReporterFactory> {
	factory_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clone()
}

/// Whether a UI factory is registered (the render path uses it to decide whether to install a silent reporter).
pub fn has_reporter_factory() -> bool {
	reporter_factory().is_some()
}

/// Progress callback: the signature matches `quercusplugin_progress_fn` in
/// `include/plugin/instance.h` verbatim — `(progress, userdata)`, where a
/// non-zero return means abort (the skeleton declared
/// `(userdata, progress) -> bool`, which disagrees with the header; the
/// header wins).
pub type ProgressFn = unsafe extern "C" fn(progress: f64, userdata: *mut std::ffi::c_void) -> c_int;

/// Progress reporter. The render path holds one, and the Progress suite's
/// progressStart/Update/End are forwarded here.
pub struct ProgressReporter {
	callback: Option<ProgressFn>,
	userdata: usize,
	/// UI reporter (created by progressStart through [`reporter_factory`]; the C
	/// callback takes precedence and it is used only when there is no callback).
	ui: Mutex<Option<Box<dyn UiProgressReporter>>>,
	/// Cancellation flag (sticky; set when update returns false).
	cancelled: AtomicBool,
}

impl ProgressReporter {
	/// No callback (the render proceeds silently; progressStart may still install
	/// a UI reporter through the factory).
	pub fn silent() -> Self {
		Self {
			callback: None,
			userdata: 0,
			ui: Mutex::new(None),
			cancelled: AtomicBool::new(false),
		}
	}

	/// With a callback.
	///
	/// # Safety
	/// `userdata` is guaranteed by the registering party to be valid for the
	/// reporter's lifetime and to obey the callback's contract; the raw pointer
	/// is stored as a `usize` (so `ProgressReporter` can be sent across threads),
	/// and the callback is still executed synchronously on the calling thread by
	/// [`Self::update`]. The silent path without a C callback uses
	/// [`Self::silent`].
	pub unsafe fn new(callback: ProgressFn, userdata: *mut std::ffi::c_void) -> Self {
		Self {
			callback: Some(callback),
			// usize 存（裸指针破坏 Send 推导；值语义不变）。
			userdata: userdata as usize,
			ui: Mutex::new(None),
			cancelled: AtomicBool::new(false),
		}
	}

	/// progressStart hook: when there is no C callback and a factory is
	/// registered, create the UI reporter on the spot (do not create it again if
	/// already installed — progressStart brackets may nest).
	pub(crate) fn install_ui(&self, label: &str, message: &str) {
		if self.callback.is_some() {
			return;
		}
		let mut slot = self.ui.lock().unwrap_or_else(|e| e.into_inner());
		if slot.is_some() {
			return;
		}
		if let Some(factory) = reporter_factory() {
			*slot = Some(factory(label, message));
		}
	}

	/// Report progress; a false return means cancellation should happen (mapping
	/// to kOfxStatReplyNo / action failure is up to the call site).
	///
	/// `progress` is a 0.0..=1.0 progress value, forwarded as-is to the current
	/// channel: with a C callback, call it (a non-zero return = abort); otherwise
	/// forward to the [`UiProgressReporter`] installed by progressStart through
	/// the factory; with neither, return true directly. A cancellation from
	/// either channel sets the cancellation flag (sticky), after which
	/// [`Self::is_cancelled`] stays true.
	pub fn update(&self, progress: f64) -> bool {
		match self.callback {
			Some(cb) => {
				// 头文件契约：非 0 = 中止。
				let abort = unsafe { cb(progress, self.userdata as *mut std::ffi::c_void) } != 0;
				if abort {
					self.cancelled.store(true, Ordering::Relaxed);
				}
				!abort
			}
			None => {
				let mut slot = self.ui.lock().unwrap_or_else(|e| e.into_inner());
				match slot.as_mut() {
					Some(ui) => {
						let keep_going = ui.update(progress);
						if !keep_going {
							self.cancelled.store(true, Ordering::Relaxed);
						}
						keep_going
					}
					None => true,
				}
			}
		}
	}

	/// Whether this render has already been cancelled (the image effect suite's abort passthrough).
	pub(crate) fn is_cancelled(&self) -> bool {
		self.cancelled.load(Ordering::Relaxed)
	}

	/// progressEnd hook: forwards to the UI reporter (no-op when there is no reporter).
	pub(crate) fn end_ui(&self) {
		let mut slot = self.ui.lock().unwrap_or_else(|e| e.into_inner());
		if let Some(ui) = slot.as_mut() {
			ui.end();
		}
	}
}
