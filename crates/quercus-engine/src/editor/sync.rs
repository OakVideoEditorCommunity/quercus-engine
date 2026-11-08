//! Read-only clip metadata used by synchronization eligibility and placement.

use quercus_node::footage::FootageBehavior;
use quercus_node::id::NodeId;

use super::EditorSessionHandle;
use crate::codec::copy_str_out;
use crate::coretypes::QuercusRational;
use crate::handle::NativeMirror;

/// POD timing and source-timecode metadata for one timeline block.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusEditorClipSyncInfo {
	pub timeline_in: QuercusRational,
	pub timeline_out: QuercusRational,
	pub media_in: QuercusRational,
	pub source_start_time: QuercusRational,
	pub has_source_start_time: bool,
	pub media_length_seconds: f64,
}

/// Reads one block's synchronization metadata without exposing graph nodes.
///
/// Source footage is resolved through the block's upstream input graph. Blocks
/// without footage still return their timeline range but report no source start.
#[unsafe(no_mangle)]
/// Reads one block's synchronization metadata without exposing graph nodes.
///
/// Resolves the first upstream footage node through the block's input graph and
/// fills `out` with the block's timeline range (in/out), its media-in point, the
/// footage's source start time plus a `has_source_start_time` flag, and the block's
/// duration in seconds. Blocks without footage still return their timeline range
/// but report no source start time.
///
/// # Returns
///
/// `false` when `session`, `out`, or `clip` is invalid, or when the node is not a
/// timeline block with a known range; `true` when the struct was fully written.
///
/// # Safety
///
/// `session` must be null or point to a live [`EditorSessionHandle`]; `out` must be
/// null or point to a writable [`QuercusEditorClipSyncInfo`]. Both must stay valid
/// for the call.
pub unsafe extern "C" fn quercus_editor_clip_sync_info(
	session: *const EditorSessionHandle,
	clip: u64,
	out: *mut QuercusEditorClipSyncInfo,
) -> bool {
	let (Some(session), Some(clip), Some(out)) = (
		unsafe { session.as_ref() },
		NodeId::from_identity(clip),
		unsafe { out.as_mut() },
	) else {
		return false;
	};
	let state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
	let project = state.project.lock().unwrap_or_else(|e| e.into_inner());
	let Some((timeline_in, timeline_out, media_in)) =
		crate::editor_commands::clip_range(&project.graph, clip)
	else {
		return false;
	};
	let source = crate::editor_commands::find_input_footage(&project.graph, clip)
		.and_then(|footage| project.graph.get(footage))
		.and_then(|entry| entry.behavior.as_any())
		.and_then(|behavior| behavior.downcast_ref::<FootageBehavior>());
	*out = QuercusEditorClipSyncInfo {
		timeline_in: QuercusRational::from_native(&timeline_in),
		timeline_out: QuercusRational::from_native(&timeline_out),
		media_in: QuercusRational::from_native(&media_in),
		source_start_time: source
			.map(|footage| QuercusRational::from_native(&footage.source_start_time))
			.unwrap_or_default(),
		has_source_start_time: source.is_some_and(|footage| footage.has_source_start_time),
		media_length_seconds: (timeline_out - timeline_in).to_f64(),
	};
	true
}

/// Returns the media filename of the first upstream footage node.
#[unsafe(no_mangle)]
/// Copies the media filename of the first upstream footage node of `clip` into
/// `buf` and returns its byte length; -1 when the session or clip is invalid or the
/// block has no upstream footage.
///
/// The filename is resolved through the block's input graph and copied
/// `snprintf`-style: NUL-terminated, truncated to `buf_len - 1` bytes, with the
/// full length returned even on truncation; a null `buf` or `buf_len == 0` queries
/// the length.
///
/// # Safety
///
/// `session` must be null or point to a live [`EditorSessionHandle`]; `buf` must be
/// null or point to `buf_len` writable bytes valid for the call.
pub unsafe extern "C" fn quercus_editor_clip_media_filename(
	session: *const EditorSessionHandle,
	clip: u64,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let (Some(session), Some(clip)) = (unsafe { session.as_ref() }, NodeId::from_identity(clip))
	else {
		return -1;
	};
	let project = session.project();
	let guard = project.lock().unwrap_or_else(|e| e.into_inner());
	crate::editor_commands::clip_media_filename(&guard.graph, clip)
		.map(|filename| copy_str_out(&filename, buf, buf_len))
		.unwrap_or(-1)
}
