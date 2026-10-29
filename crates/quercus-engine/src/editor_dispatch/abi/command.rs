//! Conversion from borrowed native ABI slots to owned semantic commands.

use crate::codec::str_arg;
use crate::node::QUERCUS_NODE_ID_INVALID;

use super::super::EditCommand;
use super::{QuercusEditOp, QuercusEditParams};

/// Copies an optional UTF-8 byte slice into an owned string for command dispatch.
pub(super) fn text(ptr: *const u8, len: usize) -> String {
	unsafe { str_arg(ptr, len) }.unwrap_or_default().to_string()
}

/// Decodes borrowed ABI storage into an owned command before executing it.
/// This keeps no caller pointers in the domain layer and validates list lengths
/// indirectly through the resulting command's domain checks.
pub(super) fn command(op: QuercusEditOp, p: &QuercusEditParams) -> Result<EditCommand, String> {
	let ids = if p.ids.is_null() {
		Vec::new()
	} else {
		unsafe { std::slice::from_raw_parts(p.ids, p.ids_len) }.to_vec()
	};
	let placements = if p.placements.is_null() {
		Vec::new()
	} else {
		unsafe { std::slice::from_raw_parts(p.placements, p.placements_len) }
			.iter()
			.map(|placement| (placement.kind, placement.index))
			.collect()
	};
	let values = if p.values.is_null() {
		Vec::new()
	} else {
		unsafe { std::slice::from_raw_parts(p.values, p.values_len) }.to_vec()
	};
	Ok(match op {
		QuercusEditOp::AddTrack => EditCommand::AddTrack { kind: p.kind },
		QuercusEditOp::RemoveTrack => EditCommand::RemoveTrack { track: p.a },
		QuercusEditOp::SetTrackHeight => EditCommand::SetTrackHeight {
			track: p.a,
			height: p.height,
		},
		QuercusEditOp::SetTrackMuted => EditCommand::SetTrackMuted {
			track: p.a,
			muted: p.enabled,
		},
		QuercusEditOp::SetTrackLocked => EditCommand::SetTrackLocked {
			track: p.a,
			locked: p.enabled,
		},
		QuercusEditOp::CreateSequence => EditCommand::CreateSequence {
			name: text(p.name, p.name_len),
			width: (p.kind != 0).then_some(p.a as i32),
			height: (p.kind != 0).then_some(p.b as i32),
			rate: (p.kind != 0).then_some(p.rate),
			interlaced: (p.kind != 0).then_some(p.enabled),
		},
		QuercusEditOp::SetSequenceParameters => EditCommand::SetSequenceParameters {
			sequence: p.a,
			name: text(p.name, p.name_len),
			width: p.b as i32,
			height: p.c as i32,
			rate: p.rate,
			interlaced: p.enabled,
		},
		QuercusEditOp::SetSequencePlayhead => EditCommand::SetSequencePlayhead {
			sequence: p.a,
			time: p.rate,
		},
		QuercusEditOp::CreateFolder => EditCommand::CreateFolder {
			name: text(p.name, p.name_len),
		},
		QuercusEditOp::CreateTextFootage => EditCommand::CreateTextFootage,
		QuercusEditOp::ImportFootage => EditCommand::ImportFootage {
			path: text(p.path, p.path_len),
		},
		QuercusEditOp::EnsureSequencesMounted => EditCommand::EnsureSequencesMounted,
		QuercusEditOp::ReprobeUnprobedFootage => EditCommand::ReprobeUnprobedFootage,
		QuercusEditOp::CreateAdjustmentLayer => EditCommand::CreateAdjustmentLayer {
			track: p.a,
			in_ts: p.in_ts,
			out_ts: p.out_ts,
		},
		QuercusEditOp::PlaceFootageClip => EditCommand::PlaceFootageClip {
			footage: p.a,
			kind: p.kind,
			track_index: p.track_index,
			in_ts: p.in_ts,
			out_ts: p.out_ts,
			media_in_ts: p.media_in_ts,
		},
		QuercusEditOp::PlaceNestedSequenceClip => EditCommand::PlaceNestedSequenceClip {
			source: p.a,
			track_index: p.track_index,
			in_ts: p.in_ts,
			out_ts: p.out_ts,
		},
		QuercusEditOp::PlaceTextClip => EditCommand::PlaceTextClip {
			text: p.a,
			track_index: p.track_index,
			in_ts: p.in_ts,
			out_ts: p.out_ts,
		},
		QuercusEditOp::PlaceGeneratorClip => EditCommand::PlaceGeneratorClip {
			generator: p.a,
			track_index: p.track_index,
			in_ts: p.in_ts,
			out_ts: p.out_ts,
		},
		QuercusEditOp::PlaceFootageClipsLinked => EditCommand::PlaceFootageClipsLinked {
			footage: p.a,
			placements,
			in_ts: p.in_ts,
			out_ts: p.out_ts,
			media_in_ts: p.media_in_ts,
		},
		QuercusEditOp::AddTransitionAtSeam => EditCommand::AddTransitionAtSeam {
			previous: p.a,
			next: p.b,
			half: p.rate,
		},
		QuercusEditOp::AddTransitionAtEdge => EditCommand::AddTransitionAtEdge {
			clip: p.a,
			start_edge: p.start_edge,
			length: p.rate,
		},
		QuercusEditOp::AddDefaultTransition => EditCommand::AddDefaultTransition {
			clips: ids,
			half: p.rate,
		},
		QuercusEditOp::SetTransitionLength => EditCommand::SetTransitionLength {
			clip: p.a,
			start_edge: p.start_edge,
			frame: p.range.in_,
			length: p.rate,
		},
		QuercusEditOp::SplitClip => EditCommand::SplitClip {
			clip: p.a,
			time_ts: p.in_ts,
		},
		QuercusEditOp::SplitClipsPreservingLinks => EditCommand::SplitClipsPreservingLinks {
			clips: ids,
			time_ts: p.in_ts,
		},
		QuercusEditOp::TrimClip => EditCommand::TrimClip {
			clip: p.a,
			new_in_ts: p.in_ts,
			new_out_ts: p.out_ts,
		},
		QuercusEditOp::RippleTrimClip => EditCommand::RippleTrimClip {
			clip: p.a,
			start_edge: p.start_edge,
			new_frame: p.new_frame,
		},
		QuercusEditOp::RollEdit => EditCommand::RollEdit {
			track: p.a,
			a: p.b,
			b: p.c,
			new_frame: p.new_frame,
		},
		QuercusEditOp::SlideClip => EditCommand::SlideClip {
			clip: p.a,
			new_start: p.new_frame,
		},
		QuercusEditOp::SlipClip => EditCommand::SlipClip {
			clip: p.a,
			new_media_in: p.new_frame,
		},
		QuercusEditOp::MoveClip => EditCommand::MoveClip {
			clip: p.a,
			new_in_ts: p.new_frame,
		},
		QuercusEditOp::MoveClipToTrack => EditCommand::MoveClipToTrack {
			clip: p.a,
			track: p.b,
			new_in_ts: p.new_frame,
		},
		QuercusEditOp::MoveClipWithLinks => EditCommand::MoveClipWithLinks {
			clip: p.a,
			track: (p.b != QUERCUS_NODE_ID_INVALID).then_some(p.b),
			new_in_ts: p.new_frame,
			linked: ids,
		},
		QuercusEditOp::SetClipsLinked => EditCommand::SetClipsLinked {
			clips: ids,
			linked: p.linked,
		},
		QuercusEditOp::DeleteClip => EditCommand::DeleteClip { clip: p.a },
		QuercusEditOp::RippleDeleteClip => EditCommand::RippleDeleteClip { clip: p.a },
		QuercusEditOp::DeleteArea => EditCommand::DeleteArea {
			in_ts: p.in_ts,
			out_ts: p.out_ts,
			ripple: p.ripple,
		},
		QuercusEditOp::RollClipEdgeTo => EditCommand::RollClipEdgeTo {
			clip: p.a,
			at_in: p.start_edge,
			frame_ts: p.new_frame,
		},
		QuercusEditOp::MarkerAdd => EditCommand::MarkerAdd {
			time: p.rate,
			name: text(p.name, p.name_len),
			color: p.kind,
		},
		QuercusEditOp::MarkerRemove => EditCommand::MarkerRemove { time: p.rate },
		QuercusEditOp::WorkareaSet => EditCommand::WorkareaSet {
			enabled: p.enabled,
			range: p.range,
		},
		QuercusEditOp::WorkareaSetUndoable => EditCommand::WorkareaSetUndoable {
			enabled: p.enabled,
			range: p.range,
			old_range: p.old_range,
		},
		QuercusEditOp::RemoveNode => EditCommand::RemoveNode { node: p.a },
		QuercusEditOp::Connect => EditCommand::Connect {
			from: p.a,
			to: p.b,
			input_id: text(p.input_id, p.input_id_len),
		},
		QuercusEditOp::Disconnect => EditCommand::Disconnect {
			to: p.a,
			input_id: text(p.input_id, p.input_id_len),
		},
		QuercusEditOp::SetContextPosition => EditCommand::SetContextPosition {
			node: p.a,
			context: p.b,
			x: p.x,
			y: p.y,
		},
		QuercusEditOp::CopyClips => EditCommand::CopyClips { clips: ids },
		QuercusEditOp::PasteClips => EditCommand::PasteClips {
			playhead_ts: p.in_ts,
		},
		QuercusEditOp::SetBlocksEnabled => EditCommand::SetBlocksEnabled {
			clips: ids,
			enabled: p.enabled,
		},
		QuercusEditOp::SetClipsSpeed => EditCommand::SetClipsSpeed {
			clips: ids,
			speed: p.speed,
		},
		QuercusEditOp::RenameBlocks => EditCommand::RenameBlocks {
			clips: ids,
			name: text(p.name, p.name_len),
		},
		QuercusEditOp::DuplicateClips => EditCommand::DuplicateClips { clips: ids },
		QuercusEditOp::PasteClipsInsert => EditCommand::PasteClipsInsert {
			playhead_ts: p.in_ts,
		},
		QuercusEditOp::NestClips => EditCommand::NestClips { clips: ids },
		QuercusEditOp::EffectInsert => EditCommand::EffectInsert {
			host: p.a,
			index: p.track_index,
			type_id: text(p.path, p.path_len),
		},
		QuercusEditOp::EffectRemove => EditCommand::EffectRemove {
			host: p.a,
			effect: p.b,
		},
		QuercusEditOp::EffectMove => EditCommand::EffectMove {
			host: p.a,
			effect: p.b,
			new_index: p.track_index,
		},
		QuercusEditOp::EffectSetEnabled => EditCommand::EffectSetEnabled {
			effect: p.a,
			enabled: p.enabled,
		},
		QuercusEditOp::SetNodeStandardValue => EditCommand::SetNodeStandardValue {
			node: p.a,
			input_id: text(p.input_id, p.input_id_len),
			element: p.element,
			value: p.node_value,
		},
		QuercusEditOp::UpsertNodeKeyframe => EditCommand::UpsertNodeKeyframe {
			node: p.a,
			input_id: text(p.input_id, p.input_id_len),
			element: p.element,
			time: p.rate,
			value: p.node_value,
			interpolation: p.kind,
			bezier_in: (p.bezier_in_x, p.bezier_in_y),
			bezier_out: (p.bezier_out_x, p.bezier_out_y),
		},
		QuercusEditOp::RemoveNodeKeyframe => EditCommand::RemoveNodeKeyframe {
			node: p.a,
			input_id: text(p.input_id, p.input_id_len),
			element: p.element,
			time: p.rate,
		},
		QuercusEditOp::SetNodeValueAtTime => EditCommand::SetNodeValueAtTime {
			node: p.a,
			input_id: text(p.input_id, p.input_id_len),
			element: p.element,
			time: p.rate,
			value: p.node_value,
		},
		QuercusEditOp::PushNodeButton => EditCommand::PushNodeButton {
			node: p.a,
			input_id: text(p.input_id, p.input_id_len),
		},
		QuercusEditOp::SetNodeTextValue => EditCommand::SetNodeTextValue {
			node: p.a,
			input_id: text(p.input_id, p.input_id_len),
			element: p.element,
			value: text(p.name, p.name_len),
		},
		QuercusEditOp::ReplaceFootage => EditCommand::ReplaceFootage {
			footage: p.a,
			path: text(p.path, p.path_len),
		},
		QuercusEditOp::DeleteEntry => EditCommand::DeleteEntry { node: p.a },
		QuercusEditOp::SetFootageProxyParams => EditCommand::SetFootageProxyParams {
			footage: p.a,
			params: p.proxy_params.clone(),
		},
		QuercusEditOp::ClearFootageProxyParams => EditCommand::ClearFootageProxyParams { footage: p.a },
		QuercusEditOp::CreateSequenceFromFootage => EditCommand::CreateSequenceFromFootage {
			footage: p.a,
			name: text(p.name, p.name_len),
		},
		QuercusEditOp::DeleteProxy => EditCommand::DeleteProxy { footage: p.a },
		QuercusEditOp::SetProxyEnabled => EditCommand::SetProxyEnabled {
			footage: p.a,
			enabled: p.enabled,
		},
		QuercusEditOp::SetMulticamEnabled => EditCommand::SetMulticamEnabled {
			clips: ids,
			enabled: p.enabled,
		},
		QuercusEditOp::SwitchMulticam => EditCommand::SwitchMulticam {
			clip: p.a,
			source: p.kind,
			split_clip: p.enabled,
			playhead: p.rate,
		},
		QuercusEditOp::CreateMulticamSequence => EditCommand::CreateMulticamSequence {
			host_sequence: p.a,
			angles: ids,
			offsets: values,
			name: text(p.name, p.name_len),
		},
	})
}
