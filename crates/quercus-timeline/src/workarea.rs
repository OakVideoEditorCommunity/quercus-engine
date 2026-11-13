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

//! Timeline work area (`src/timeline/src/timelineworkarea.h` +
//! `timelineundoworkarea.h`): the in/out range, its enabled flag, and the
//! `WorkareaSetEnabledCommand`/`WorkareaSetRangeCommand` undo commands.
//!
//! De-Qt: no QObject, no signals — change notifications are the facade's
//! job.

use std::sync::{Arc, Mutex};

use quercus_core::{Rational, TimeRange};
use quercus_undo::undocommand::UndoCommand;

use crate::handle::get;

/// The reset sentinel `k_reset_in` (timelineworkarea.h): 0/1. Exposed as a
/// function because `Rational::new` is not yet `const` in quercuscore-rs.
pub fn reset_in() -> Rational {
	Rational::new(0, 1)
}
/// The reset sentinel `k_reset_out` (timelineworkarea.h): RATIONAL_MAX,
/// i.e. 2147483647/1.
pub fn reset_out() -> Rational {
	Rational::new(2147483647, 1)
}

/// `TimelineWorkArea` — the in/out range on a timeline (timelineworkarea.h).
pub struct TimelineWorkArea {
	/// Whether the work area is enabled.
	workarea_enabled_: bool,
	/// The in/out range.
	workarea_range_: TimeRange,
}

impl TimelineWorkArea {
	/// A new, disabled work area at `k_reset_in`..`k_reset_out`.
	pub fn new() -> Self {
		Self {
			workarea_enabled_: false,
			workarea_range_: TimeRange::new(reset_in(), reset_out()),
		}
	}

	/// Whether the work area is enabled.
	pub fn enabled(&self) -> bool {
		self.workarea_enabled_
	}

	/// Set enabled.
	///
	/// `e` is stored as-is: enabling does not validate or clamp the range, and
	/// disabling keeps the current range so re-enabling restores it. Default is
	/// `false` (see [`Self::new`]); `WorkareaSetEnabledCommand` wraps this for
	/// undo/redo.
	pub fn set_enabled(&mut self, e: bool) {
		self.workarea_enabled_ = e;
	}

	/// The in point.
	pub fn in_(&self) -> Rational {
		self.workarea_range_.in_()
	}

	/// The out point.
	///
	/// Returns `k_reset_out` (2147483647/1) while the work area has never had a
	/// range set, so consumers should check [`Self::enabled`] before treating the
	/// value as a real edit point. Owned rational; see [`Self::in_`] for the
	/// matching bound.
	pub fn out(&self) -> Rational {
		self.workarea_range_.out()
	}

	/// The range length.
	///
	/// Computed as `out - in`, so it is negative or zero if the bounds were set
	/// out of order; with the untouched reset sentinels it spans the whole
	/// `k_reset_in..k_reset_out` range. Returns an owned rational.
	pub fn length(&self) -> Rational {
		self.workarea_range_.length()
	}

	/// The full range.
	///
	/// Returns the complete in/out pair, including the `k_reset_in`/`k_reset_out`
	/// sentinels when the range was never set. The borrow is shared; use
	/// [`Self::in_`] / [`Self::out`] for a single bound or [`Self::set_range`] to
	/// change it.
	pub fn range(&self) -> &TimeRange {
		&self.workarea_range_
	}

	/// Set the range.
	///
	/// `range` replaces the in/out pair as-is: no ordering, length or clamp is
	/// enforced here, and the enabled flag is untouched. This is the raw setter
	/// used by `WorkareaSetRangeCommand`; playback consumers read the work area
	/// only when it is enabled.
	pub fn set_range(&mut self, range: TimeRange) {
		self.workarea_range_ = range;
	}
}

/// `WorkareaSetEnabledCommand` (timelineundoworkarea.h).
pub struct WorkareaSetEnabledCommand {
	/// Target work area (shared behind the facade's value handle; `None`
	/// for an empty/null handle, which makes the command a no-op).
	points: Option<Arc<Mutex<TimelineWorkArea>>>,
	/// New enabled flag.
	new_enabled: bool,
	/// Enabled flag captured at construction, restored by `undo`.
	old_enabled: bool,
}

impl WorkareaSetEnabledCommand {
	/// Construct from work area + new enabled value (captures old at ctor).
	///
	/// `points` is the facade's work-area handle (the shared work area is cloned
	/// out of it; a null/empty handle yields a no-op command) and `enabled` is
	/// the flag applied on redo. The current flag is captured as `old_enabled`
	/// (defaulting to `false` for a null handle).
	pub fn new(points: crate::handle::CHandle, enabled: bool) -> Self {
		// SAFETY: work-area handles box an `Arc<Mutex<TimelineWorkArea>>`
		// (created by `make_owned`); reading it clones the shared `Arc`.
		let wa = unsafe { get::<Arc<Mutex<TimelineWorkArea>>>(&points) }.cloned();
		let old_enabled = wa
			.as_ref()
			.map(|w| {
				let w = w.lock().unwrap_or_else(|e| e.into_inner());
				w.enabled()
			})
			.unwrap_or(false);
		Self {
			points: wa,
			new_enabled: enabled,
			old_enabled,
		}
	}

	/// Construct over an already-shared work area (the crate's Rust-typed
	/// entry; the facade-facing [`Self::new`] forwards here).
	///
	/// `points` is the shared target and `enabled` the flag applied on redo; the
	/// current flag is read under the lock and captured as `old_enabled` for
	/// undo. A poisoned lock is recovered rather than propagated.
	pub fn new_arc(points: Arc<Mutex<TimelineWorkArea>>, enabled: bool) -> Self {
		let old_enabled = points.lock().unwrap_or_else(|e| e.into_inner()).enabled();
		Self {
			points: Some(points),
			new_enabled: enabled,
			old_enabled,
		}
	}

	/// `redo`: set enabled.
	///
	/// Writes the captured `new_enabled` into the shared work area under its
	/// lock; with a null/empty handle (`points` = `None`) this is a no-op, and
	/// re-applying the same flag is idempotent.
	pub fn redo(&mut self) {
		if let Some(wa) = &self.points {
			let mut wa = wa.lock().unwrap_or_else(|e| e.into_inner());
			wa.set_enabled(self.new_enabled);
		}
	}

	/// `undo`: restore old enabled.
	pub fn undo(&mut self) {
		if let Some(wa) = &self.points {
			let mut wa = wa.lock().unwrap_or_else(|e| e.into_inner());
			wa.set_enabled(self.old_enabled);
		}
	}

	/// Wrap as an quercusundo command handle.
	pub fn to_command(self) -> UndoCommand {
		crate::undocommon::box_command(self)
	}
}

impl crate::undocommon::Command for WorkareaSetEnabledCommand {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}

/// `WorkareaSetRangeCommand` (timelineundoworkarea.h). The old range is
/// captured at construction when not supplied.
pub struct WorkareaSetRangeCommand {
	/// Target work area (shared behind the facade's value handle; `None`
	/// for an empty/null handle, which makes the command a no-op).
	workarea: Option<Arc<Mutex<TimelineWorkArea>>>,
	/// New range.
	new_range: TimeRange,
	/// Range captured at construction, restored by `undo`.
	old_range: TimeRange,
}

impl WorkareaSetRangeCommand {
	/// Construct from work area + new range (captures current as old).
	///
	/// `workarea` is the facade's work-area handle (the shared work area is
	/// cloned out of it; a null/empty handle yields a no-op command) and `range`
	/// is the range applied on redo. The current range is captured as
	/// `old_range`; when the handle is null that capture falls back to `range`
	/// itself, which is harmless because redo/undo then do nothing.
	pub fn new(workarea: crate::handle::CHandle, range: TimeRange) -> Self {
		// SAFETY: as `WorkareaSetEnabledCommand::new`.
		let wa = unsafe { get::<Arc<Mutex<TimelineWorkArea>>>(&workarea) }.cloned();
		let old_range = wa
			.as_ref()
			.map(|w| {
				let w = w.lock().unwrap_or_else(|e| e.into_inner());
				*w.range()
			})
			.unwrap_or(range);
		Self::new_with_old_opt(wa, range, old_range)
	}

	/// Construct over an already-shared work area (the crate's Rust-typed
	/// entry; the facade-facing [`Self::new`] forwards here).
	///
	/// `workarea` is the shared target and `range` the range applied on redo;
	/// the current range is read under the lock and captured as `old_range` for
	/// undo. A poisoned lock is recovered (the inner value is used), so this
	/// cannot panic.
	pub fn new_arc(workarea: Arc<Mutex<TimelineWorkArea>>, range: TimeRange) -> Self {
		let old_range = *workarea.lock().unwrap_or_else(|e| e.into_inner()).range();
		Self::new_with_old_opt(Some(workarea), range, old_range)
	}

	/// Construct from work area + new range + explicitly supplied old
	/// range (used by callers that may have a previously captured range;
	/// `new` delegates here with the current range).
	pub fn new_with_old(
		workarea: crate::handle::CHandle,
		range: TimeRange,
		old_range: TimeRange,
	) -> Self {
		// SAFETY: as `WorkareaSetEnabledCommand::new`.
		let wa = unsafe { get::<Arc<Mutex<TimelineWorkArea>>>(&workarea) }.cloned();
		Self::new_with_old_opt(wa, range, old_range)
	}

	/// Construct over an already-shared work area with an explicitly
	/// supplied old range (the crate's Rust-typed entry; the facade-facing
	/// [`Self::new_with_old`] forwards here).
	pub fn new_with_old_arc(
		workarea: Arc<Mutex<TimelineWorkArea>>,
		range: TimeRange,
		old_range: TimeRange,
	) -> Self {
		Self::new_with_old_opt(Some(workarea), range, old_range)
	}

	/// Shared constructor over an optional work area (`None` mirrors the
	/// empty/null handle: the command is a no-op).
	///
	/// `workarea` is the shared target (optional), `range` the range applied by
	/// [`Self::redo`] and `old_range` the range restored by [`Self::undo`].
	/// This is where every `new*` constructor of this command ends up; it never
	/// locks, so no state is observed or mutated here.
	fn new_with_old_opt(
		workarea: Option<Arc<Mutex<TimelineWorkArea>>>,
		range: TimeRange,
		old_range: TimeRange,
	) -> Self {
		Self {
			workarea,
			new_range: range,
			old_range,
		}
	}

	/// `redo`: set the range.
	///
	/// Writes the captured `new_range` into the shared work area under its lock;
	/// with a null/empty handle (`workarea` = `None`) this is a no-op. Safe to
	/// call repeatedly — re-applying the same range is idempotent.
	pub fn redo(&mut self) {
		if let Some(wa) = &self.workarea {
			let mut wa = wa.lock().unwrap_or_else(|e| e.into_inner());
			wa.set_range(self.new_range);
		}
	}

	/// `undo`: restore the old range.
	pub fn undo(&mut self) {
		if let Some(wa) = &self.workarea {
			let mut wa = wa.lock().unwrap_or_else(|e| e.into_inner());
			wa.set_range(self.old_range);
		}
	}

	/// Wrap as an quercusundo command handle.
	pub fn to_command(self) -> UndoCommand {
		crate::undocommon::box_command(self)
	}
}

impl crate::undocommon::Command for WorkareaSetRangeCommand {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}
