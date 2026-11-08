//! Read-only multicam graph queries for editor sessions.

use quercus_node::graph::Graph;
use quercus_node::id::NodeId;
use quercus_node::nodes::multicamnode::{CURRENT_INPUT, MultiCamNode, SEQUENCE_TYPE_INPUT};
use quercus_node::track::TrackType;

use super::EditorSessionHandle;
use crate::codec::copy_str_out;
use crate::coretypes::QuercusRational;
use crate::editor_commands as ops;
use crate::handle::NativeMirror;
use crate::node::QUERCUS_NODE_ID_INVALID;

/// ABI mirror of the semantic multicam state returned by editor commands.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusEditorMulticamState {
	pub sequence: u64,
	pub multicam: u64,
	pub clip: u64,
	pub source_count: i32,
	pub current_source: i32,
}

impl NativeMirror for QuercusEditorMulticamState {
	type Native = ops::MulticamState;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			sequence: native.sequence.identity(),
			multicam: native.multicam.identity(),
			clip: native.clip.identity(),
			source_count: native.source_count,
			current_source: native.current_source,
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			sequence: NodeId::from_identity(self.sequence).unwrap_or(NodeId::INVALID),
			multicam: NodeId::from_identity(self.multicam).unwrap_or(NodeId::INVALID),
			clip: NodeId::from_identity(self.clip).unwrap_or(NodeId::INVALID),
			source_count: self.source_count,
			current_source: self.current_source,
		}
	}
}

/// Resolves a clip's multicam state: multicam node, source sequence, angle count
/// and current angle.
///
/// On success the resolved state is written through `out`; on failure `out` is
/// left untouched. Returns `false` when `session` or `out` is null, when `clip`
/// is not a valid node id, when the clip is not multicam-enabled, or when its
/// multicam node has no connected source sequence.
///
/// # Safety
///
/// `session` must be a live handle from `quercus_editor_new` (or null); `out`
/// must be valid for one `QuercusEditorMulticamState` write when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_state(
	session: *const EditorSessionHandle,
	clip: u64,
	out: *mut QuercusEditorMulticamState,
) -> bool {
	let (Some(session), Some(clip), Some(out)) = (
		unsafe { session.as_ref() },
		NodeId::from_identity(clip),
		unsafe { out.as_mut() },
	) else {
		return false;
	};
	let project = session.project();
	let Some(state) = ops::multicam_state(&project, clip) else {
		return false;
	};
	*out = QuercusEditorMulticamState::from_native(&state);
	true
}

/// Returns the first sequence feeding a clip's texture chain.
///
/// The clip's `texture_in` source is returned when it is itself a sequence;
/// otherwise the upstream input graph is walked depth-first and the first
/// sequence found wins. Returns the sequence's node identity, or
/// `QUERCUS_NODE_ID_INVALID` when the session or clip id is invalid or no
/// sequence is reachable.
///
/// # Safety
///
/// `session` must be a live handle from `quercus_editor_new`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_connected_sequence(
	session: *const EditorSessionHandle,
	clip: u64,
) -> u64 {
	let (Some(session), Some(clip)) = (unsafe { session.as_ref() }, NodeId::from_identity(clip))
	else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let project = session.project();
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	ops::clip_connected_sequence(&guard.graph, clip)
		.map(|sequence| sequence.identity())
		.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// Whether a clip's direct texture source is a multicam node.
///
/// Only the node wired to the clip's `texture_in` input is checked — one hop,
/// not a walk — so a multicam buried further upstream is not reported. Returns
/// `false` for a null session or an invalid node id.
///
/// # Safety
///
/// `session` must be a live handle from `quercus_editor_new`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_is_clip(
	session: *const EditorSessionHandle,
	clip: u64,
) -> bool {
	let (Some(session), Some(clip)) = (unsafe { session.as_ref() }, NodeId::from_identity(clip))
	else {
		return false;
	};
	let project = session.project();
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	ops::clip_is_multicam(&guard.graph, clip)
}

/// Finds the multicam clip covering `time` on a sequence's video tracks.
///
/// `time` is a position in the sequence's timebase (a `QuercusRational` passed by
/// value); a clip matches when `clip.in <= time < clip.out`. Video tracks are
/// scanned in order and each track's blocks in track order, and the first block
/// whose clip has multicam enabled is returned. Returns the clip's node identity,
/// or `QUERCUS_NODE_ID_INVALID` when the session or sequence id is invalid or no
/// multicam clip covers `time`.
///
/// # Safety
///
/// `session` must be a live handle from `quercus_editor_new`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_clip_at(
	session: *const EditorSessionHandle,
	sequence: u64,
	time: QuercusRational,
) -> u64 {
	let (Some(session), Some(sequence)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(sequence))
	else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let project = session.project();
	ops::multicam_clip_at(&project, sequence, time.to_native())
		.map(|clip| clip.identity())
		.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// Returns the source sequence connected to a multicam node.
///
/// That sequence's track list supplies the angles (see
/// [`quercus_editor_multicam_source_count`] and
/// [`quercus_editor_multicam_source_track`]). Returns the sequence's node
/// identity, or `QUERCUS_NODE_ID_INVALID` when the session or node id is invalid,
/// or when nothing is connected to the node's `sequence_in` input.
///
/// # Safety
///
/// `session` must be a live handle from `quercus_editor_new`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_sequence(
	session: *const EditorSessionHandle,
	multicam: u64,
) -> u64 {
	let (Some(session), Some(multicam)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(multicam))
	else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let project = session.project();
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	ops::multicam_sequence(&guard.graph, multicam)
		.map(|sequence| sequence.identity())
		.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// Returns one angle's track inside the multicam node's source sequence.
///
/// `source` is a zero-based index into the source sequence's track list of the
/// type selected by `sequence_type_in` (video by default), i.e. the same list
/// counted by [`quercus_editor_multicam_source_count`]. Returns the track's node
/// identity, or `QUERCUS_NODE_ID_INVALID` when the session or node id is invalid,
/// `source < 0`, no source sequence is connected, or `source` is past the end of
/// the track list.
///
/// # Safety
///
/// `session` must be a live handle from `quercus_editor_new`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_source_track(
	session: *const EditorSessionHandle,
	multicam: u64,
	source: i32,
) -> u64 {
	let (Some(session), Some(multicam)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(multicam))
	else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let project = session.project();
	ops::multicam_source_track(&project, multicam, source)
		.map(|track| track.identity())
		.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// Reads the multicam node's source track type (`sequence_type_in`, default
/// video), mirroring `editor_commands::multicam`.
fn source_track_type(graph: &Graph, multicam: NodeId) -> TrackType {
	graph
		.get(multicam)
		.map(|entry| {
			let value = entry
				.core
				.standard_value(SEQUENCE_TYPE_INPUT, -1)
				.to_double() as i32;
			TrackType::from_c(value).unwrap_or(TrackType::Video)
		})
		.unwrap_or(TrackType::Video)
}

/// Number of tracks in the multicam node's connected source track list.
fn source_count(graph: &Graph, multicam: NodeId) -> i32 {
	let Some(sequence) = ops::multicam_sequence(graph, multicam) else {
		return 0;
	};
	ops::track_list_of(graph, sequence, source_track_type(graph, multicam))
		.and_then(|list| ops::track_list_behavior(graph, list))
		.map(|list| list.tracks.len() as i32)
		.unwrap_or(0)
}

/// Reads the multicam node's `current_in` standard value.
///
/// The combo is stored as a double and truncated to `i32`, defaulting to `-1`
/// when the input is missing; also `-1` when `multicam` is not in `graph`.
/// Mirrors `editor_commands::multicam::current_source` so the FFI and the command
/// layer agree.
fn current_source(graph: &Graph, multicam: NodeId) -> i32 {
	graph
		.get(multicam)
		.map(|entry| entry.core.standard_value(CURRENT_INPUT, -1).to_double() as i32)
		.unwrap_or(-1)
}

/// Returns the number of angles (source tracks) of a multicam node.
///
/// Counts the tracks of the node's connected source sequence in the track list
/// selected by `sequence_type_in` (video by default), so it is `0` when no
/// sequence is connected or the list is unreadable. Returns `-1` for a null
/// session, an invalid node id, or an id no longer in the graph.
///
/// # Safety
///
/// `session` must be a live handle from `quercus_editor_new`, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_source_count(
	session: *const EditorSessionHandle,
	multicam: u64,
) -> i32 {
	let (Some(session), Some(multicam)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(multicam))
	else {
		return -1;
	};
	let project = session.project();
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	if guard.graph.get(multicam).is_none() {
		return -1;
	}
	source_count(&guard.graph, multicam)
}

/// Returns the multicam node's currently selected angle.
///
/// Reads the node's `current_in` standard value (a combo stored as a double,
/// truncated to `i32`), which defaults to `-1` when the input is missing.
/// Returns `-1` for a null session, an invalid node id, or an id that is no
/// longer present in the project graph.
///
/// # Safety
///
/// `session` must be a live handle from `quercus_editor_new`, or null (which is
/// rejected).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_current_source(
	session: *const EditorSessionHandle,
	multicam: u64,
) -> i32 {
	let (Some(session), Some(multicam)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(multicam))
	else {
		return -1;
	};
	let project = session.project();
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	if guard.graph.get(multicam).is_none() {
		return -1;
	}
	current_source(&guard.graph, multicam)
}

/// Copies one angle's display name into the caller's buffer.
///
/// The name is `"Video Track {n}"`, `"Audio Track {n}"` or `"Subtitle Track {n}"`
/// from the angle track's kind and stored index (`n` falls back to `source` when
/// the stored index is negative). When `buf` is non-null and `buf_len > 0` the
/// name is copied and NUL-terminated, truncated to `buf_len - 1` bytes; the full
/// name length in bytes is returned regardless, so a null/zero buffer queries the
/// length. Returns `-1` for a null session, an invalid node id, a negative
/// `source`, no connected source sequence, or a `source` past the end of the
/// track list.
///
/// # Safety
///
/// `session` must be a live handle from `quercus_editor_new`, or null; `buf`
/// must be valid for `buf_len` byte writes when non-null. The buffer is borrowed
/// for the call only and stays owned by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_source_name(
	session: *const EditorSessionHandle,
	multicam: u64,
	source: i32,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let (Some(session), Some(multicam)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(multicam))
	else {
		return -1;
	};
	if source < 0 {
		return -1;
	}
	let project = session.project();
	let Some(track) = ops::multicam_source_track(&project, multicam, source) else {
		return -1;
	};
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	let Some(track_behavior) = ops::track_behavior(&guard.graph, track) else {
		return -1;
	};
	let index = if track_behavior.index >= 0 {
		track_behavior.index
	} else {
		source
	};
	let name = match track_behavior.kind {
		TrackType::Video => format!("Video Track {index}"),
		TrackType::Audio => format!("Audio Track {index}"),
		TrackType::Subtitle => format!("Subtitle Track {index}"),
	};
	copy_str_out(&name, buf, buf_len)
}

/// Computes the square-ish angle grid used to display `source_count` angles.
///
/// The smaller dimension grows first (`1x1`, `1x2`, `2x2`, `2x3`, ...), matching
/// `MultiCamNode::rows_and_columns`; `rows`/`cols` receive the result and are the
/// same grid [`quercus_editor_multicam_source_at`] hit-tests. Returns `false`
/// without writing anything when either out-pointer is null or
/// `source_count <= 0`.
///
/// # Safety
///
/// `rows` and `cols` must each be valid for one `i32` write when non-null and
/// must not alias each other; null is accepted and rejected with `false`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_grid(
	source_count: i32,
	rows: *mut i32,
	cols: *mut i32,
) -> bool {
	let (Some(rows), Some(cols)) = (unsafe { rows.as_mut() }, unsafe { cols.as_mut() }) else {
		return false;
	};
	if source_count <= 0 {
		return false;
	}
	let (row_count, col_count) = MultiCamNode::rows_and_columns(source_count);
	*rows = row_count;
	*cols = col_count;
	true
}

/// Hit-tests normalized display coordinates against the multicam angle grid.
///
/// `source_count` is the number of angles; `x`/`y` are normalized display
/// coordinates in `0.0..=1.0` from the top-left. The grid comes from
/// `MultiCamNode::rows_and_columns(source_count)` and the cell is
/// `(floor(y * max(rows, cols)), floor(x * max(rows, cols)))`. Returns the angle
/// index in `0..source_count`, or `-1` for a non-positive count, a coordinate
/// outside `0..=1` (NaN included), a point outside the grid (the unused part of
/// a non-square grid), or an index past the last angle.
///
/// Port of C++ `MulticamDisplay::DisplayClicked`; no session or project is
/// needed.
///
/// # Safety
///
/// No pointers are involved and there are no preconditions: the `unsafe`
/// qualifier only marks the C ABI boundary, where callers may pass any `f64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_multicam_source_at(
	source_count: i32,
	x: f64,
	y: f64,
) -> i32 {
	if source_count <= 0 || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
		return -1;
	}
	let (rows, cols) = MultiCamNode::rows_and_columns(source_count);
	let multi = rows.max(cols);
	let col = (x * f64::from(multi)).floor() as i32;
	let row = (y * f64::from(multi)).floor() as i32;
	if col >= cols || row >= rows {
		return -1;
	}
	let index = MultiCamNode::rows_cols_to_index(row, col, rows, cols);
	if index >= source_count {
		return -1;
	}
	index
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The C entry points must reject null/invalid handles before touching the
	/// project; these are the defensive branches every UI caller relies on.
	#[test]
	fn null_and_invalid_inputs_are_rejected() {
		let session = crate::editor::quercus_editor_new();
		assert!(!session.is_null());
		let mut state = QuercusEditorMulticamState::default();
		unsafe {
			assert!(!quercus_editor_multicam_state(
				std::ptr::null(),
				NodeId::INVALID.identity(),
				&mut state
			));
			assert!(!quercus_editor_multicam_state(session, u64::MAX, &mut state));
			assert!(!quercus_editor_multicam_state(
				session,
				u64::MAX,
				std::ptr::null_mut()
			));
			assert_eq!(
				quercus_editor_multicam_connected_sequence(std::ptr::null(), 0),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(
				quercus_editor_multicam_connected_sequence(session, u64::MAX),
				QUERCUS_NODE_ID_INVALID
			);
			assert!(!quercus_editor_multicam_is_clip(std::ptr::null(), 0));
			assert!(!quercus_editor_multicam_is_clip(session, u64::MAX));
			assert_eq!(
				quercus_editor_multicam_clip_at(std::ptr::null(), 0, QuercusRational::default()),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(
				quercus_editor_multicam_clip_at(session, u64::MAX, QuercusRational::default()),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(
				quercus_editor_multicam_sequence(std::ptr::null(), 0),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(
				quercus_editor_multicam_sequence(session, u64::MAX),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(
				quercus_editor_multicam_source_track(std::ptr::null(), 0, 0),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(
				quercus_editor_multicam_source_track(session, u64::MAX, 0),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(quercus_editor_multicam_source_count(std::ptr::null(), 0), -1);
			assert_eq!(quercus_editor_multicam_source_count(session, u64::MAX), -1);
			assert_eq!(
				quercus_editor_multicam_current_source(std::ptr::null(), 0),
				-1
			);
			assert_eq!(quercus_editor_multicam_current_source(session, u64::MAX), -1);
			assert_eq!(
				quercus_editor_multicam_source_name(std::ptr::null(), 0, 0, std::ptr::null_mut(), 0),
				-1
			);
			assert_eq!(
				quercus_editor_multicam_source_name(session, u64::MAX, -1, std::ptr::null_mut(), 0),
				-1
			);
			assert_eq!(
				quercus_editor_multicam_source_name(session, u64::MAX, 0, std::ptr::null_mut(), 0),
				-1
			);
			let mut rows = 0;
			let mut cols = 0;
			assert!(!quercus_editor_multicam_grid(0, &mut rows, &mut cols));
			assert!(!quercus_editor_multicam_grid(3, std::ptr::null_mut(), &mut cols));
			assert!(!quercus_editor_multicam_grid(3, &mut rows, std::ptr::null_mut()));
			assert!(!quercus_editor_multicam_grid(3, std::ptr::null_mut(), std::ptr::null_mut()));
			assert_eq!(quercus_editor_multicam_source_at(0, 0.0, 0.0), -1);
			assert_eq!(quercus_editor_multicam_source_at(3, -0.1, 0.0), -1);
			assert_eq!(quercus_editor_multicam_source_at(3, 0.0, 1.1), -1);
			assert_eq!(quercus_editor_multicam_source_at(3, 0.99, 0.99), -1);
			assert_eq!(quercus_editor_multicam_source_at(3, f64::NAN, 0.0), -1);
			crate::editor::quercus_editor_release(session);
		}
	}
	/// Exercise the valid-session early returns: a non-multicam node, a
	/// multicam node without a sequence, and a valid id absent from the graph.
	#[test]
	fn valid_session_invalid_graph_paths() {
		let session = crate::editor::quercus_editor_new();
		assert!(!session.is_null());
		let project = unsafe { &*session }.project();
		let (multicam, plain_clip, missing) = {
			let mut guard = project.lock().unwrap();
			let (core, behavior) = quercus_node::nodes::multicamnode::create();
			let multicam = guard.graph.add_node(core, behavior);
			let (core, behavior) = quercus_node::block::clip_create();
			let plain_clip = guard.graph.add_node(core, behavior);
			let missing = NodeId::from_identity(999_999).unwrap();
			(multicam, plain_clip, missing)
		};
		let mut state = QuercusEditorMulticamState::default();
		unsafe {
			// Valid session + non-multicam clip: multicam_state returns false.
			assert!(!quercus_editor_multicam_state(
				session,
				plain_clip.identity(),
				&mut state
			));
			// Valid session + valid clip but null output: false before touching it.
			assert!(!quercus_editor_multicam_state(
				session,
				plain_clip.identity(),
				std::ptr::null_mut()
			));
			// Valid multicam without sequence: count 0, current 0, name -1.
			assert_eq!(
				quercus_editor_multicam_source_count(session, multicam.identity()),
				0
			);
			assert_eq!(
				quercus_editor_multicam_current_source(session, multicam.identity()),
				0
			);
			assert_eq!(
				quercus_editor_multicam_source_name(
					session,
					multicam.identity(),
					0,
					std::ptr::null_mut(),
					0
				),
				-1
			);
			// Valid identity that is no longer in the graph: early -1 path.
			assert_eq!(
				quercus_editor_multicam_source_count(session, missing.identity()),
				-1
			);
			assert_eq!(
				quercus_editor_multicam_current_source(session, missing.identity()),
				-1
			);
			assert_eq!(
				quercus_editor_multicam_source_name(
					session,
					missing.identity(),
					0,
					std::ptr::null_mut(),
					0
				),
				-1
			);
			// 1x2 grid: a point below the first row is outside the grid.
			assert_eq!(quercus_editor_multicam_source_at(2, 0.25, 0.75), -1);
			// Grid outputs for a valid count.
			let mut rows = 0;
			let mut cols = 0;
			assert!(quercus_editor_multicam_grid(2, &mut rows, &mut cols));
			assert_eq!((rows, cols), (1, 2));
			crate::editor::quercus_editor_release(session);
		}
	}

}
