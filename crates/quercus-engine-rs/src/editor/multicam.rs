//! Safe multicam state and graph-resolution queries.

use std::ffi::c_void;
use std::ptr;

use crate::node::NodeId;
use crate::types::Rational;

use super::EditorSession;

#[cfg_attr(
	all(windows, feature = "engine-cdylib"),
	windows,
	link(
		name = "quercus_engine.dll.lib",
		kind = "dylib",
		modifiers = "+verbatim"
	)
)]
#[cfg_attr(
	all(not(windows), feature = "engine-cdylib"),
	link(name = "quercus_engine", kind = "dylib")
)]
unsafe extern "C" {
	fn quercus_editor_multicam_state(
		session: *const c_void,
		clip: u64,
		out: *mut RawMulticamState,
	) -> bool;
	fn quercus_editor_multicam_connected_sequence(session: *const c_void, clip: u64) -> u64;
	fn quercus_editor_multicam_is_clip(session: *const c_void, clip: u64) -> bool;
	fn quercus_editor_multicam_clip_at(
		session: *const c_void,
		sequence: u64,
		time: Rational,
	) -> u64;
	fn quercus_editor_multicam_sequence(session: *const c_void, multicam: u64) -> u64;
	fn quercus_editor_multicam_source_track(
		session: *const c_void,
		multicam: u64,
		source: i32,
	) -> u64;
	fn quercus_editor_multicam_source_count(session: *const c_void, multicam: u64) -> i32;
	fn quercus_editor_multicam_current_source(session: *const c_void, multicam: u64) -> i32;
	fn quercus_editor_multicam_source_name(
		session: *const c_void,
		multicam: u64,
		source: i32,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	fn quercus_editor_multicam_grid(source_count: i32, rows: *mut i32, cols: *mut i32) -> bool;
	fn quercus_editor_multicam_source_at(source_count: i32, x: f64, y: f64) -> i32;
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct RawMulticamState {
	sequence: u64,
	multicam: u64,
	clip: u64,
	source_count: i32,
	current_source: i32,
}
/// Resolved multicam node, source sequence and current angle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MulticamState {
	/// The `sequence` field.
	pub sequence: NodeId,
	/// The `multicam` field.
	pub multicam: NodeId,
	/// The `clip` field.
	pub clip: NodeId,
	/// The `source_count` field.
	pub source_count: i32,
	/// The `current_source` field.
	pub current_source: i32,
}
/// One multicam angle: index, source-sequence track, display name and
/// selection state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MulticamSource {
	/// Zero-based angle index in the multicam node's source track list.
	pub index: i32,
	/// The angle's track inside the multicam node's source sequence.
	pub track: NodeId,
	/// Display name (`"Video Track N"`, `"Audio Track N"`, ...).
	pub name: String,
	/// Whether this angle is the multicam node's current source.
	pub current: bool,
}
impl EditorSession {
	/// Resolve a clip's multicam state: its source sequence, the multicam node,
	/// the angle count and the current angle.
	///
	/// Returns `None` when the clip is invalid or multicam is not enabled on it
	/// (the C ABI returns `false`). The state is copied into an owned
	/// [`MulticamState`] of plain [`NodeId`]s, so the result does not borrow the
	/// session.
	pub fn multicam_state(&self, clip: NodeId) -> Option<MulticamState> {
		let mut raw = RawMulticamState::default();
		unsafe { quercus_editor_multicam_state(self.ptr, clip.raw(), &mut raw) }.then(|| {
			MulticamState {
				sequence: NodeId::from_raw(raw.sequence),
				multicam: NodeId::from_raw(raw.multicam),
				clip: NodeId::from_raw(raw.clip),
				source_count: raw.source_count,
				current_source: raw.current_source,
			}
		})
	}

	/// Returns the first sequence feeding the clip's texture chain.
	pub fn multicam_connected_sequence(&self, clip: NodeId) -> Option<NodeId> {
		let id = NodeId::from_raw(unsafe {
			quercus_editor_multicam_connected_sequence(self.ptr, clip.raw())
		});
		id.is_valid().then_some(id)
	}

	/// Whether a clip's *direct* texture source is a multicam node.
	///
	/// Only the immediate source is inspected: a clip whose multicam node sits
	/// behind another node reports `false`. Use
	/// [`EditorSession::multicam_connected_sequence`] to walk the whole texture
	/// chain instead, or [`EditorSession::multicam_state`] for the resolved
	/// state.
	pub fn multicam_is_clip(&self, clip: NodeId) -> bool {
		unsafe { quercus_editor_multicam_is_clip(self.ptr, clip.raw()) }
	}

	/// Finds the multicam clip covering `time` on a sequence's video tracks.
	pub fn multicam_clip_at(&self, sequence: NodeId, time: Rational) -> Option<NodeId> {
		let id = NodeId::from_raw(unsafe {
			quercus_editor_multicam_clip_at(self.ptr, sequence.raw(), time)
		});
		id.is_valid().then_some(id)
	}

	/// The source sequence connected to a multicam node, if it resolves.
	///
	/// Returns `None` when `multicam` is invalid or is not a multicam node (the
	/// C side reports an invalid id). The sequence holds the angles' tracks;
	/// look up one angle with [`EditorSession::multicam_source_track`] or list
	/// them all with [`EditorSession::multicam_sources`].
	pub fn multicam_sequence(&self, multicam: NodeId) -> Option<NodeId> {
		let id =
			NodeId::from_raw(unsafe { quercus_editor_multicam_sequence(self.ptr, multicam.raw()) });
		id.is_valid().then_some(id)
	}

	/// Returns one angle's track inside the connected source sequence.
	pub fn multicam_source_track(&self, multicam: NodeId, source: i32) -> Option<NodeId> {
		let id = NodeId::from_raw(unsafe {
			quercus_editor_multicam_source_track(self.ptr, multicam.raw(), source)
		});
		id.is_valid().then_some(id)
	}

	/// Lists a multicam node's angles, skipping entries that cannot be
	/// resolved.
	pub fn multicam_sources(&self, multicam: NodeId) -> Vec<MulticamSource> {
		let Some(count) = self.multicam_source_count(multicam) else {
			return Vec::new();
		};
		let current = self.multicam_current_source(multicam);
		let mut sources = Vec::new();
		for index in 0..count {
			let Some(track) = self.multicam_source_track(multicam, index) else {
				continue;
			};
			let Some(name) = self.multicam_source_name(multicam, index) else {
				continue;
			};
			sources.push(MulticamSource {
				index,
				track,
				name,
				current: current == Some(index),
			});
		}
		sources
	}

	/// Returns a multicam node's angle count, or `None` when it is invalid.
	pub fn multicam_source_count(&self, multicam: NodeId) -> Option<i32> {
		let count = unsafe { quercus_editor_multicam_source_count(self.ptr, multicam.raw()) };
		(count >= 0).then_some(count)
	}

	/// Returns a multicam node's current angle, or `None` when it is invalid.
	pub fn multicam_current_source(&self, multicam: NodeId) -> Option<i32> {
		let current = unsafe { quercus_editor_multicam_current_source(self.ptr, multicam.raw()) };
		(current >= 0).then_some(current)
	}

	/// Angle grid layout `(rows, cols)` for `sources`, or `None` when the
	/// count is not positive.
	pub fn multicam_grid(sources: i32) -> Option<(i32, i32)> {
		let mut rows = 0;
		let mut cols = 0;
		unsafe { quercus_editor_multicam_grid(sources, &mut rows, &mut cols) }
			.then_some((rows, cols))
	}

	/// Hit-test normalized display coordinates against the angle grid.
	///
	/// `sources` is the angle count the grid was built for (see
	/// [`EditorSession::multicam_grid`]); `x`/`y` are pointer coordinates
	/// normalized to `0..=1` with the origin at the top-left of the viewer.
	/// Returns the zero-based index of the angle under the pointer, or `None`
	/// outside the grid or on a cell with no angle (the C side reports a
	/// negative index).
	pub fn multicam_source_at(sources: i32, x: f64, y: f64) -> Option<i32> {
		let index = unsafe { quercus_editor_multicam_source_at(sources, x, y) };
		(index >= 0).then_some(index)
	}

	/// Query one angle's display name through the C ABI.
	///
	/// `multicam` is the multicam node and `source` the zero-based angle index.
	/// Calls the C function twice: first with a null buffer to measure, then
	/// with a `measured + 1` byte buffer, truncating to the reported length and
	/// decoding as UTF-8. Returns `None` when the C side reports a negative
	/// length (invalid node or angle) or when the bytes are not valid UTF-8;
	/// used by [`EditorSession::multicam_sources`].
	fn multicam_source_name(&self, multicam: NodeId, source: i32) -> Option<String> {
		let len = unsafe {
			quercus_editor_multicam_source_name(
				self.ptr,
				multicam.raw(),
				source,
				ptr::null_mut(),
				0,
			)
		};
		if len < 0 {
			return None;
		}
		let mut buf = vec![0u8; len as usize + 1];
		let written = unsafe {
			quercus_editor_multicam_source_name(
				self.ptr,
				multicam.raw(),
				source,
				buf.as_mut_ptr(),
				buf.len(),
			)
		};
		if written < 0 {
			return None;
		}
		buf.truncate(written as usize);
		String::from_utf8(buf).ok()
	}
}

#[cfg(test)]
mod tests;

/// Choose a near-square angle grid for `sources` cells, as `(rows, cols)`.
///
/// Grows the smaller dimension alternately, so `rows * cols >= sources` (1 →
/// `(1, 1)`, 4 → `(2, 2)`, 5 → `(2, 3)`); a non-positive count yields
/// `(1, 1)`. Pairs with [`rows_cols_to_index`] for layout and hit-testing of
/// the multicam angle grid.
pub fn rows_and_columns(sources: i32) -> (i32, i32) {
	let mut rows = 1;
	let mut cols = 1;
	while rows * cols < sources {
		if rows < cols {
			rows += 1;
		} else {
			cols += 1;
		}
	}
	(rows, cols)
}

/// Convert a grid row/column into the index of the angle it displays.
///
/// Row-major arithmetic: `col + row * total_cols`. `total_rows` is accepted for
/// symmetry with [`rows_and_columns`] but is not needed by the formula and is
/// ignored. Cells past the angle count still produce an index, so callers must
/// bound-check the result against the number of angles before using it.
pub fn rows_cols_to_index(row: i32, col: i32, total_rows: i32, total_cols: i32) -> i32 {
	let _ = total_rows;
	col + row * total_cols
}
