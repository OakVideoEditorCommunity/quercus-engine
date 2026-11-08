//! C ABI additions for quercus-undo (prefix `quercus_undo_`): history
//! navigation/inspection and change observers.
//!
//! The base surface (can_undo/can_redo/count/index,
//! group_begin/end/abort, undo/redo) lives in [`crate::node`] next to
//! the project API; this module completes it with history jumping,
//! clearing, row inspection (the history panel's data source), and the
//! observer hook functional plugins use as the "project modified"
//! signal.

use std::ffi::c_void;
use std::sync::Mutex;

use crate::codec::{copy_str_out, str_arg};
use quercus_undo::undocommand::UndoCommand;

/// Jumps to history position `index` (undoing/redoing as needed).
/// False on failure.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_undo_jump(index: i64) -> bool {
	quercus_undo::global::jump(index).is_ok()
}

/// Clears the undo stack. False on failure.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_undo_clear() -> bool {
	quercus_undo::global::clear().is_ok()
}

/// Take ownership of one boxed undo command and record it on the process-wide
/// undo stack with the given display name.
///
/// This is the migration path for commands built outside the Rust API: the
/// caller hands over a raw `Box<UndoCommand>` and the engine pushes it as if it
/// had been created here (running its redo, then notifying observers). `name`
/// need not be NUL-terminated and is copied; it is the label shown in the
/// history panel.
///
/// Ownership is consumed as soon as `command` is non-null, *even when this
/// function returns `false`*: the box is reconstructed immediately and the
/// command is either stored on the stack or dropped while unwinding the error
/// (invalid name, or a push that the stack rejects). The caller must therefore
/// never use, free, or re-push `command` again, whatever the result.
///
/// # Returns
///
/// `true` when the command was recorded; `false` when `command` is null (in
/// which case no ownership is taken), when `name` is null or not valid UTF-8,
/// or when the stack rejected the push.
///
/// # Safety
///
/// `command` must be null or a pointer returned by `Box::into_raw` for a
/// `Box<UndoCommand>` that has not been freed and is not aliased elsewhere. It
/// must be valid to pass to `Box::from_raw` exactly once. `name` must be null or
/// point to `name_len` readable bytes for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_undo_push_owned(
	command: *mut c_void,
	name: *const u8,
	name_len: usize,
) -> bool {
	if command.is_null() {
		return false;
	}
	let command = unsafe { Box::from_raw(command as *mut UndoCommand) };
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return false;
	};
	quercus_undo::global::push(*command, name).is_ok()
}

/// Write the display name of history row `row` into `buf`, snprintf-style.
///
/// `row` is the 0-based index into the undo stack, oldest command first; the
/// string is the label the history panel shows for that command. The copy
/// semantics are those of `snprintf`: the name is written NUL-terminated and
/// truncated to `buf_len - 1` bytes when it does not fit, and the return value is
/// the name's full byte length excluding the NUL, so `ret >= buf_len` means the
/// output was truncated. A null `buf` or `buf_len == 0` writes nothing and just
/// reports the length (the two-stage length query).
///
/// # Returns
///
/// The required byte length (excluding the NUL) on success, or `-1` when `row`
/// is out of range.
///
/// # Safety
///
/// `buf` must be null or valid for writing `buf_len` bytes for the duration of
/// the call; a non-null `buf` with `buf_len == 0` is accepted and writes nothing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_undo_command_name(row: i64, buf: *mut u8, buf_len: usize) -> i32 {
	match quercus_undo::global::command_name(row) {
		Ok(name) => copy_str_out(&name, buf, buf_len),
		Err(_) => -1,
	}
}

/// Whether history row `row` is currently in the done state: 0/1, -1
/// when out of range.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_undo_command_done(row: i64) -> i32 {
	match quercus_undo::global::command_done(row) {
		Ok(done) => done as i32,
		Err(_) => -1,
	}
}

/// A change observer: called after any undoable command lands (push,
/// undo, redo, jump, clear).
pub type QuercusUndoObserverFn = unsafe extern "C" fn(userdata: *mut c_void);

// The registry holds (callback, userdata) pairs; the userdata pointer
// is opaque consumer state kept alive by the caller for the
// registration's lifetime (there is no removal — matching
// quercus_undo::global::add_observer).
static OBSERVERS: Mutex<Vec<(QuercusUndoObserverFn, usize)>> = Mutex::new(Vec::new());

// Registered with quercus_undo once, on the first observer registration.
static TRAMPOLINE_ONCE: std::sync::Once = std::sync::Once::new();

fn dispatch_observers() {
	let observers = OBSERVERS.lock().unwrap_or_else(|e| e.into_inner());
	for (cb, userdata) in observers.iter() {
		unsafe { cb(*userdata as *mut c_void) };
	}
}

/// Register a callback invoked when the process-wide undo stack changes.
///
/// Observers are the "project modified" signal for functional plugins: they run
/// after a command is recorded on the stack (`push` outside an open group),
/// after a group commit (`group_end`), and after a successful `jump` — which
/// covers undo and redo. They do **not** fire for pushes inside an open group,
/// for `group_abort`, or for `clear`.
///
/// Registration is one-way: there is no removal (matching
/// `quercus_undo::global::add_observer`), so a registered pair stays for the
/// rest of the process. The first non-null registration installs the internal
/// dispatcher exactly once; later calls only append. A null `cb` is a no-op.
///
/// `userdata` is opaque consumer state, passed back to `cb` unchanged and never
/// dereferenced here; it must stay valid for the rest of the process. The
/// callback runs on whichever thread mutated the stack and is invoked while the
/// observer registry is locked, so it must not call this function (that would
/// deadlock on the same mutex) and should not block.
///
/// # Safety
///
/// `userdata` must remain valid to hand to `cb` for as long as the process
/// lives, and `cb` must be safe to call from any thread with the registry
/// mutex held. Both are the caller's contract; nothing here can verify them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_undo_add_observer(
	cb: Option<QuercusUndoObserverFn>,
	userdata: *mut c_void,
) {
	let Some(cb) = cb else {
		return;
	};
	TRAMPOLINE_ONCE.call_once(|| {
		quercus_undo::global::add_observer(dispatch_observers);
	});
	OBSERVERS
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.push((cb, userdata as usize));
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::{AtomicUsize, Ordering};

	#[test]
	fn history_rows_and_observer() {
		// Serialize: the undo stack is process-global (shared with the
		// node tests).
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		static SEEN: AtomicUsize = AtomicUsize::new(0);
		unsafe extern "C" fn observer(userdata: *mut c_void) {
			assert!(userdata.is_null());
			SEEN.fetch_add(1, Ordering::SeqCst);
		}
		unsafe { quercus_undo_add_observer(Some(observer), std::ptr::null_mut()) };

		unsafe {
			let project = crate::node::quercus_project_new();
			let base = crate::node::quercus_undo_count();
			let (n, nl) = ("A Folder".as_ptr(), 8);
			let folder = crate::node::quercus_folder_create(
				project,
				crate::node::quercus_project_root(project),
				n,
				nl,
			);
			assert!(crate::node::quercus_node_id_is_valid(folder));
			assert_eq!(crate::node::quercus_undo_count(), base + 1);
			assert!(SEEN.load(Ordering::SeqCst) > 0, "the observer fired");

			// Row inspection: the newest row is the folder creation.
			let mut buf = [0u8; 64];
			let len = quercus_undo_command_name(base, buf.as_mut_ptr(), buf.len());
			assert!(len > 0);
			assert_eq!(quercus_undo_command_done(base), 1);
			assert_eq!(quercus_undo_command_name(-1, buf.as_mut_ptr(), 64), -1);

			// Jump back to base and forward again.
			assert!(quercus_undo_jump(base));
			assert_eq!(quercus_undo_command_done(base), 0);
			assert!(quercus_undo_jump(base + 1));
			assert_eq!(quercus_undo_command_done(base), 1);

			crate::node::quercus_project_release(project);
			assert!(quercus_undo_clear());
			// clear() pushes a fresh empty command (the "New Project" row).
			assert_eq!(crate::node::quercus_undo_count(), 1);
		}
	}
}
