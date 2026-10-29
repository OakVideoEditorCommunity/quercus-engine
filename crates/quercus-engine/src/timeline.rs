//! C ABI for quercus-timeline (prefix `quercus_timeline_`): the undoable
//! timeline edit commands.
//!
//! Each entry point constructs the command and pushes it onto the
//! global undo stack — pushing executes it (C++ semantics), so every
//! call is one undoable edit step (group several with
//! `quercus_undo_group_begin`/`end`). These are the `edit.*` primitives a
//! functional plugin builds on (split, ripple delete, trim, move,
//! enable/disable, track add/remove).
//!
//! Nodes are addressed by the same u64 identity tokens as the
//! `quercus_node_*` API; times are `QuercusRational` seconds. All entry points
//! fail closed (false) on null handles, unknown ids, or a rejected
//! command.

use quercus_node::id::NodeId;
use quercus_undo::undocommand::UndoCommand;

use crate::coretypes::QuercusRational;
use crate::handle::NativeMirror;
use crate::node::ProjectHandle;

/// Builds an quercus-timeline node reference from a project handle and a
/// node id token.
fn node_ref(project: *const ProjectHandle, id: u64) -> Option<quercus_timeline::util::NodeRef> {
	if project.is_null() {
		return None;
	}
	Some(quercus_timeline::util::NodeRef {
		project: unsafe { &*project }.project().clone(),
		id: NodeId::from_identity(id)?,
	})
}

/// Pushes the command onto the global undo stack (executing it).
fn push(cmd: UndoCommand, name: &str) -> bool {
	quercus_undo::global::push(cmd, name).is_ok()
}

/// Splits a block at `point` (seconds, project timebase). The second
/// half becomes a new block (re-query the track for it).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_block_split(
	project: *const ProjectHandle,
	block: u64,
	point: QuercusRational,
) -> bool {
	let Some(block) = node_ref(project, block) else {
		return false;
	};
	push(
		quercus_timeline::undosplit::BlockSplitCommand::new(block, point.to_native()).to_command(),
		"Split Clip",
	)
}

/// Splits every block of `blocks` at `point` as ONE undoable command
/// (originally linked pairs keep their rear halves linked — the Cmd+K
/// semantics).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_split_preserving_links(
	project: *const ProjectHandle,
	blocks: *const u64,
	count: usize,
	point: QuercusRational,
) -> bool {
	if project.is_null() || blocks.is_null() || count == 0 {
		return false;
	}
	let point = point.to_native();
	let ids = unsafe { std::slice::from_raw_parts(blocks, count) };
	let mut refs = Vec::with_capacity(count);
	for &id in ids {
		let Some(r) = node_ref(project, id) else {
			return false;
		};
		refs.push(r);
	}
	let times = vec![point; refs.len()];
	push(
		quercus_timeline::undosplit::BlockSplitPreservingLinksCommand::new(refs, times).to_command(),
		"Split Clips",
	)
}

/// Splits every block of a track at `point`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_track_split_at(
	project: *const ProjectHandle,
	track: u64,
	point: QuercusRational,
) -> bool {
	let Some(track) = node_ref(project, track) else {
		return false;
	};
	push(
		quercus_timeline::undosplit::TrackSplitAtTimeCommand::new(track, point.to_native())
			.to_command(),
		"Split Track",
	)
}

/// Ripple-removes `[in_, out)` from every track of a timeline (the
/// sequence node), closing the gap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_ripple_remove_area(
	project: *const ProjectHandle,
	timeline: u64,
	in_: QuercusRational,
	out: QuercusRational,
) -> bool {
	let Some(timeline) = node_ref(project, timeline) else {
		return false;
	};
	push(
		quercus_timeline::undoripple::TimelineRippleRemoveAreaCommand::new(
			timeline,
			in_.to_native(),
			out.to_native(),
		)
		.to_command(),
		"Ripple Delete",
	)
}

/// Resizes a block (out point moves, in anchored).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_block_resize(
	project: *const ProjectHandle,
	block: u64,
	new_length: QuercusRational,
) -> bool {
	let Some(block) = node_ref(project, block) else {
		return false;
	};
	push(
		quercus_timeline::undogeneral::BlockResizeCommand::new(block, new_length.to_native())
			.to_command(),
		"Resize Block",
	)
}

/// Resizes a block keeping the out point (the media in point shifts).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_block_resize_media_in(
	project: *const ProjectHandle,
	block: u64,
	new_length: QuercusRational,
) -> bool {
	let Some(block) = node_ref(project, block) else {
		return false;
	};
	push(
		quercus_timeline::undogeneral::BlockResizeWithMediaInCommand::new(
			block,
			new_length.to_native(),
		)
		.to_command(),
		"Resize Block",
	)
}

/// Sets a clip's media in point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_block_set_media_in(
	project: *const ProjectHandle,
	block: u64,
	media_in: QuercusRational,
) -> bool {
	let Some(block) = node_ref(project, block) else {
		return false;
	};
	push(
		quercus_timeline::undogeneral::BlockSetMediaInCommand::new(block, media_in.to_native())
			.to_command(),
		"Set Media In",
	)
}

/// Enables/disables a block.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_block_set_enabled(
	project: *const ProjectHandle,
	block: u64,
	enabled: bool,
) -> bool {
	let Some(block) = node_ref(project, block) else {
		return false;
	};
	push(
		quercus_timeline::undogeneral::BlockEnableDisableCommand::new(block, enabled).to_command(),
		if enabled {
			"Enable Block"
		} else {
			"Disable Block"
		},
	)
}

/// `quercus_timeline_block_trim` modes (`quercus_timeline::common::MovementMode`).
pub const QUERCUS_TIMELINE_TRIM_NONE: i32 = 0;
/// Move (slide the block's position).
pub const QUERCUS_TIMELINE_TRIM_MOVE: i32 = 1;
/// Trim the in point.
pub const QUERCUS_TIMELINE_TRIM_IN: i32 = 2;
/// Trim the out point.
pub const QUERCUS_TIMELINE_TRIM_OUT: i32 = 3;

/// Trims a block to `new_length` with trim semantics: `mode` selects
/// which end moves and how neighbours react (one of
/// `QUERCUS_TIMELINE_TRIM_*`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_block_trim(
	project: *const ProjectHandle,
	track: u64,
	block: u64,
	new_length: QuercusRational,
	mode: i32,
) -> bool {
	use quercus_timeline::common::MovementMode;
	let mode = match mode {
		QUERCUS_TIMELINE_TRIM_NONE => MovementMode::None,
		QUERCUS_TIMELINE_TRIM_MOVE => MovementMode::Move,
		QUERCUS_TIMELINE_TRIM_IN => MovementMode::TrimIn,
		QUERCUS_TIMELINE_TRIM_OUT => MovementMode::TrimOut,
		_ => return false,
	};
	let (Some(track), Some(block)) = (node_ref(project, track), node_ref(project, block)) else {
		return false;
	};
	push(
		quercus_timeline::undopointer::BlockTrimCommand::new(
			track,
			block,
			new_length.to_native(),
			mode,
		)
		.to_command(),
		"Trim Block",
	)
}

/// Places a block onto track `track_index` of `timeline` at `in_`
/// (moving a block across tracks / inserting at a point).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_block_place(
	project: *const ProjectHandle,
	timeline: u64,
	track_index: i32,
	block: u64,
	in_: QuercusRational,
) -> bool {
	let (Some(timeline), Some(block)) = (node_ref(project, timeline), node_ref(project, block))
	else {
		return false;
	};
	push(
		quercus_timeline::undopointer::TrackPlaceBlockCommand::new(
			timeline,
			track_index,
			block,
			in_.to_native(),
		)
		.to_command(),
		"Place Block",
	)
}

/// Moves a block to track `track_index` of `timeline` at `in_` (the
/// source slot becomes a gap).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_block_move(
	project: *const ProjectHandle,
	timeline: u64,
	track_index: i32,
	block: u64,
	in_: QuercusRational,
) -> bool {
	let (Some(timeline), Some(block)) = (node_ref(project, timeline), node_ref(project, block))
	else {
		return false;
	};
	push(
		quercus_timeline::undopointer::TrackMoveBlockCommand::new(
			timeline,
			track_index,
			block,
			in_.to_native(),
		)
		.to_command(),
		"Move Block",
	)
}

/// Adds a track to a track list (a sequence's video/audio/subtitle
/// list node — see `quercus_sequence_track_list_at`). Returns the new
/// track's node id, 0 on failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_add_track(
	project: *const ProjectHandle,
	track_list: u64,
) -> u64 {
	let Some(list) = node_ref(project, track_list) else {
		return 0;
	};
	let cmd = quercus_timeline::undogeneral::TimelineAddTrackCommand::new(list);
	let id = cmd.track().id.identity();
	if !push(cmd.to_command(), "Add Track") {
		return 0;
	}
	id
}

/// Removes a track (its blocks go with it).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_remove_track(
	project: *const ProjectHandle,
	track: u64,
) -> bool {
	let Some(track) = node_ref(project, track) else {
		return false;
	};
	push(
		quercus_timeline::undogeneral::TimelineRemoveTrackCommand::new(track).to_command(),
		"Remove Track",
	)
}

/// Sets a transition's in/out offsets.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_timeline_transition_set_offsets(
	project: *const ProjectHandle,
	block: u64,
	in_offset: QuercusRational,
	out_offset: QuercusRational,
) -> bool {
	let Some(block) = node_ref(project, block) else {
		return false;
	};
	push(
		quercus_timeline::undogeneral::TransitionSetOffsetsCommand::new(
			block,
			Some(in_offset.to_native()),
			Some(out_offset.to_native()),
		)
		.to_command(),
		"Set Transition Offsets",
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn arg(s: &str) -> (*const u8, usize) {
		(s.as_ptr(), s.len())
	}

	/// Builds a project with one clip on the video track (the engine
	/// node ABI path), returning (project, sequence, track, block).
	unsafe fn project_with_clip() -> (*mut ProjectHandle, u64, u64, u64) {
		unsafe {
			let project = crate::node::quercus_project_new();
			let (s, sl) = arg("Seq");
			let sequence = crate::node::quercus_sequence_create(project, s, sl);
			let video_list = crate::node::quercus_sequence_track_list_at(project, sequence, 0);
			let track = crate::node::quercus_tracklist_track_at(project, video_list, 0);
			assert!(crate::node::quercus_node_id_is_valid(track));

			let block = crate::node::quercus_block_create_clip(project);
			assert!(crate::node::quercus_node_id_is_valid(block));
			assert!(crate::node::quercus_track_append_block(project, track, block));
			assert!(crate::node::quercus_block_set_length(
				project,
				block,
				QuercusRational { num: 2, den: 1 }
			));
			(project, sequence, track, block)
		}
	}

	/// Split + resize + enable/disable + undo unwind, all through the
	/// C ABI.
	#[test]
	fn split_and_edit_commands_are_undoable() {
		// Serialize: the undo stack is process-global (shared with the
		// node tests).
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		unsafe {
			let (project, _seq, track, block) = project_with_clip();
			let base = crate::node::quercus_undo_count();
			let blocks_before = crate::node::quercus_track_block_count(project, track);

			// Split the clip at 0.5 s: one undo step, one more block.
			assert!(quercus_timeline_block_split(
				project,
				block,
				QuercusRational { num: 1, den: 2 }
			));
			assert_eq!(crate::node::quercus_undo_count(), base + 1);
			assert_eq!(
				crate::node::quercus_track_block_count(project, track),
				blocks_before + 1
			);

			// Resize the first block; set media in; disable it.
			let first = crate::node::quercus_track_block_at(project, track, 0);
			assert!(quercus_timeline_block_resize(
				project,
				first,
				QuercusRational { num: 2, den: 1 }
			));
			assert!(quercus_timeline_block_set_media_in(
				project,
				first,
				QuercusRational { num: 0, den: 1 }
			));
			assert!(quercus_timeline_block_set_enabled(project, first, false));
			assert_eq!(crate::node::quercus_undo_count(), base + 4);

			// Undo everything, one step at a time.
			for _ in 0..4 {
				assert!(crate::node::quercus_undo_undo());
			}
			assert_eq!(
				crate::node::quercus_track_block_count(project, track),
				blocks_before
			);
			for _ in 0..4 {
				assert!(crate::node::quercus_undo_redo());
			}
			assert_eq!(
				crate::node::quercus_track_block_count(project, track),
				blocks_before + 1
			);

			// Fail closed.
			assert!(!quercus_timeline_block_split(
				std::ptr::null(),
				block,
				QuercusRational { num: 1, den: 2 }
			));
			assert!(!quercus_timeline_block_split(
				project,
				u64::MAX,
				QuercusRational { num: 1, den: 2 }
			));

			crate::node::quercus_project_release(project);
			assert!(quercus_undo::global::clear().is_ok());
		}
	}

	/// Track add/remove through the C ABI.
	#[test]
	fn add_and_remove_track() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		unsafe {
			let project = crate::node::quercus_project_new();
			let (s, sl) = arg("Seq");
			let sequence = crate::node::quercus_sequence_create(project, s, sl);
			let video_list = crate::node::quercus_sequence_track_list_at(project, sequence, 0);
			let before = crate::node::quercus_tracklist_track_count(project, video_list);

			let track = quercus_timeline_add_track(project, video_list);
			assert!(crate::node::quercus_node_id_is_valid(track));
			assert_eq!(
				crate::node::quercus_tracklist_track_count(project, video_list),
				before + 1
			);

			assert!(quercus_timeline_remove_track(project, track));
			assert_eq!(
				crate::node::quercus_tracklist_track_count(project, video_list),
				before
			);
			assert!(!quercus_timeline_remove_track(project, u64::MAX));

			crate::node::quercus_project_release(project);
			assert!(quercus_undo::global::clear().is_ok());
		}
	}
}
