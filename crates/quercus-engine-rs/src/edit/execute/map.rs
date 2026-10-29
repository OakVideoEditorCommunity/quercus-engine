//! Mapping from safe command variants to operation-specific ABI slots.

use crate::node::NodeId;
use crate::editor::KeyframeInterpolation;
use crate::types::Rational;

use super::super::EditCommand;
use super::super::abi::*;
use super::{CommandArgs, kind_code};

impl CommandArgs {
	pub(super) fn map(&mut self, command: EditCommand) {
		match command {
			EditCommand::AddTrack { kind } => {
				self.op = QuercusEditOp::AddTrack;
				self.params.kind = kind_code(kind);
			}
			EditCommand::RemoveTrack { track } => self.simple(QuercusEditOp::RemoveTrack, track),
			EditCommand::SetTrackHeight { track, height } => {
				self.simple(QuercusEditOp::SetTrackHeight, track);
				self.params.height = height;
			}
			EditCommand::SetTrackMuted { track, muted } => {
				self.simple(QuercusEditOp::SetTrackMuted, track);
				self.params.enabled = muted;
			}
			EditCommand::SetTrackLocked { track, locked } => {
				self.simple(QuercusEditOp::SetTrackLocked, track);
				self.params.enabled = locked;
			}
			EditCommand::CreateSequence {
				name,
				width,
				height,
				rate,
				interlaced,
			} => {
				self.op = QuercusEditOp::CreateSequence;
				self.name(name);
				self.params.kind = width.is_some() as i32;
				self.params.a = width.unwrap_or(0) as u64;
				self.params.b = height.unwrap_or(0) as u64;
				self.params.rate = rate.unwrap_or(Rational::NULL);
				self.params.enabled = interlaced.unwrap_or(false);
			}
			EditCommand::SetSequenceParameters {
				sequence,
				name,
				width,
				height,
				rate,
				interlaced,
			} => {
				self.simple(QuercusEditOp::SetSequenceParameters, sequence);
				self.name(name);
				self.params.b = width as u64;
				self.params.c = height as u64;
				self.params.rate = rate;
				self.params.enabled = interlaced;
			}
			EditCommand::SetSequencePlayhead { sequence, time } => {
				self.simple(QuercusEditOp::SetSequencePlayhead, sequence);
				self.params.rate = time;
			}
			EditCommand::CreateFolder { name } => {
				self.op = QuercusEditOp::CreateFolder;
				self.name(name);
			}
			EditCommand::CreateTextFootage => self.op = QuercusEditOp::CreateTextFootage,
			EditCommand::ImportFootage { path } => {
				self.op = QuercusEditOp::ImportFootage;
				self.path(path);
			}
			EditCommand::EnsureSequencesMounted => self.op = QuercusEditOp::EnsureSequencesMounted,
			EditCommand::ReprobeUnprobedFootage => self.op = QuercusEditOp::ReprobeUnprobedFootage,
			EditCommand::CreateAdjustmentLayer {
				track,
				in_ts,
				out_ts,
			} => {
				self.simple(QuercusEditOp::CreateAdjustmentLayer, track);
				self.params.in_ts = in_ts;
				self.params.out_ts = out_ts;
			}
			EditCommand::PlaceFootageClip {
				footage,
				kind,
				track_index,
				in_ts,
				out_ts,
				media_in_ts,
			} => {
				self.simple(QuercusEditOp::PlaceFootageClip, footage);
				self.params.kind = kind_code(kind);
				self.params.track_index = track_index;
				self.params.in_ts = in_ts;
				self.params.out_ts = out_ts;
				self.params.media_in_ts = media_in_ts;
			}
			EditCommand::PlaceNestedSequenceClip {
				source,
				track_index,
				in_ts,
				out_ts,
			} => {
				self.simple(QuercusEditOp::PlaceNestedSequenceClip, source);
				self.params.track_index = track_index;
				self.params.in_ts = in_ts;
				self.params.out_ts = out_ts;
			}
			EditCommand::PlaceTextClip {
				text,
				track_index,
				in_ts,
				out_ts,
			} => {
				self.simple(QuercusEditOp::PlaceTextClip, text);
				self.params.track_index = track_index;
				self.params.in_ts = in_ts;
				self.params.out_ts = out_ts;
			}
			EditCommand::PlaceGeneratorClip {
				generator,
				track_index,
				in_ts,
				out_ts,
			} => {
				self.simple(QuercusEditOp::PlaceGeneratorClip, generator);
				self.params.track_index = track_index;
				self.params.in_ts = in_ts;
				self.params.out_ts = out_ts;
			}
			EditCommand::PlaceFootageClipsLinked {
				footage,
				placements,
				in_ts,
				out_ts,
				media_in_ts,
			} => {
				self.simple(QuercusEditOp::PlaceFootageClipsLinked, footage);
				self.placements(placements);
				self.params.in_ts = in_ts;
				self.params.out_ts = out_ts;
				self.params.media_in_ts = media_in_ts;
			}
			EditCommand::AddTransitionAtSeam {
				previous,
				next,
				half,
			} => {
				self.simple(QuercusEditOp::AddTransitionAtSeam, previous);
				self.params.b = next.raw();
				self.params.rate = half;
			}
			EditCommand::AddTransitionAtEdge {
				clip,
				start_edge,
				length,
			} => {
				self.simple(QuercusEditOp::AddTransitionAtEdge, clip);
				self.params.start_edge = start_edge;
				self.params.rate = length;
			}
			EditCommand::AddDefaultTransition { clips, half } => {
				self.op = QuercusEditOp::AddDefaultTransition;
				self.ids(clips);
				self.params.rate = half;
			}
			EditCommand::SetTransitionLength {
				clip,
				start_edge,
				frame,
				length,
			} => {
				self.simple(QuercusEditOp::SetTransitionLength, clip);
				self.params.start_edge = start_edge;
				self.params.range.in_ = frame;
				self.params.rate = length;
			}
			EditCommand::SplitClip { clip, time_ts } => {
				self.simple(QuercusEditOp::SplitClip, clip);
				self.params.in_ts = time_ts;
			}
			EditCommand::SplitClipsPreservingLinks { clips, time_ts } => {
				self.op = QuercusEditOp::SplitClipsPreservingLinks;
				self.ids(clips);
				self.params.in_ts = time_ts;
			}
			EditCommand::TrimClip {
				clip,
				new_in_ts,
				new_out_ts,
			} => {
				self.simple(QuercusEditOp::TrimClip, clip);
				self.params.in_ts = new_in_ts;
				self.params.out_ts = new_out_ts;
			}
			EditCommand::RippleTrimClip {
				clip,
				start_edge,
				new_frame,
			} => {
				self.simple(QuercusEditOp::RippleTrimClip, clip);
				self.params.start_edge = start_edge;
				self.params.new_frame = new_frame;
			}
			EditCommand::RollEdit {
				track,
				a,
				b,
				new_frame,
			} => {
				self.simple(QuercusEditOp::RollEdit, track);
				self.params.b = a.raw();
				self.params.c = b.raw();
				self.params.new_frame = new_frame;
			}
			EditCommand::SlideClip { clip, new_start } => {
				self.simple(QuercusEditOp::SlideClip, clip);
				self.params.new_frame = new_start;
			}
			EditCommand::SlipClip { clip, new_media_in } => {
				self.simple(QuercusEditOp::SlipClip, clip);
				self.params.new_frame = new_media_in;
			}
			EditCommand::MoveClip { clip, new_in_ts } => {
				self.simple(QuercusEditOp::MoveClip, clip);
				self.params.new_frame = new_in_ts;
			}
			EditCommand::MoveClipToTrack {
				clip,
				track,
				new_in_ts,
			} => {
				self.simple(QuercusEditOp::MoveClipToTrack, clip);
				self.params.b = track.raw();
				self.params.new_frame = new_in_ts;
			}
			EditCommand::MoveClipWithLinks {
				clip,
				track,
				new_in_ts,
				linked,
			} => {
				self.simple(QuercusEditOp::MoveClipWithLinks, clip);
				self.params.b = track.map_or(invalid_id(), NodeId::raw);
				self.params.new_frame = new_in_ts;
				self.ids(linked);
			}
			EditCommand::SetClipsLinked { clips, linked } => {
				self.op = QuercusEditOp::SetClipsLinked;
				self.ids(clips);
				self.params.linked = linked;
			}
			EditCommand::DeleteClip { clip } => self.simple(QuercusEditOp::DeleteClip, clip),
			EditCommand::RippleDeleteClip { clip } => {
				self.simple(QuercusEditOp::RippleDeleteClip, clip)
			}
			EditCommand::DeleteArea {
				in_ts,
				out_ts,
				ripple,
			} => {
				self.op = QuercusEditOp::DeleteArea;
				self.params.in_ts = in_ts;
				self.params.out_ts = out_ts;
				self.params.ripple = ripple;
			}
			EditCommand::RollClipEdgeTo {
				clip,
				at_in,
				frame_ts,
			} => {
				self.simple(QuercusEditOp::RollClipEdgeTo, clip);
				self.params.start_edge = at_in;
				self.params.new_frame = frame_ts;
			}
			EditCommand::MarkerAdd { time, name, color } => {
				self.op = QuercusEditOp::MarkerAdd;
				self.params.rate = time;
				self.name(name);
				self.params.kind = color;
			}
			EditCommand::MarkerRemove { time } => {
				self.op = QuercusEditOp::MarkerRemove;
				self.params.rate = time;
			}
			EditCommand::WorkareaSet { enabled, range } => {
				self.op = QuercusEditOp::WorkareaSet;
				self.params.enabled = enabled;
				self.params.range = range;
			}
			EditCommand::WorkareaSetUndoable {
				enabled,
				range,
				old_range,
			} => {
				self.op = QuercusEditOp::WorkareaSetUndoable;
				self.params.enabled = enabled;
				self.params.range = range;
				self.params.old_range = old_range;
			}
			EditCommand::RemoveNode { node } => self.simple(QuercusEditOp::RemoveNode, node),
			EditCommand::Connect { from, to, input_id } => {
				self.simple(QuercusEditOp::Connect, from);
				self.params.b = to.raw();
				self.input_id(input_id);
			}
			EditCommand::Disconnect { to, input_id } => {
				self.simple(QuercusEditOp::Disconnect, to);
				self.input_id(input_id);
			}
			EditCommand::SetContextPosition {
				node,
				context,
				x,
				y,
			} => {
				self.simple(QuercusEditOp::SetContextPosition, node);
				self.params.b = context.raw();
				self.params.x = x;
				self.params.y = y;
			}
			EditCommand::CopyClips { clips } => {
				self.op = QuercusEditOp::CopyClips;
				self.ids(clips);
			}
			EditCommand::PasteClips { playhead_ts } => {
				self.op = QuercusEditOp::PasteClips;
				self.params.in_ts = playhead_ts;
			}
			EditCommand::SetBlocksEnabled { clips, enabled } => {
				self.op = QuercusEditOp::SetBlocksEnabled;
				self.ids(clips);
				self.params.enabled = enabled;
			}
			EditCommand::SetClipsSpeed { clips, speed } => {
				self.op = QuercusEditOp::SetClipsSpeed;
				self.ids(clips);
				self.params.speed = speed;
			}
			EditCommand::RenameBlocks { clips, name } => {
				self.op = QuercusEditOp::RenameBlocks;
				self.ids(clips);
				self.name(name);
			}
			EditCommand::DuplicateClips { clips } => {
				self.op = QuercusEditOp::DuplicateClips;
				self.ids(clips);
			}
			EditCommand::PasteClipsInsert { playhead_ts } => {
				self.op = QuercusEditOp::PasteClipsInsert;
				self.params.in_ts = playhead_ts;
			}
			EditCommand::NestClips { clips } => {
				self.op = QuercusEditOp::NestClips;
				self.ids(clips);
			}
			EditCommand::EffectInsert {
				host,
				index,
				type_id,
			} => {
				self.simple(QuercusEditOp::EffectInsert, host);
				self.params.track_index = index;
				self.path(type_id);
			}
			EditCommand::EffectRemove { host, effect } => {
				self.simple(QuercusEditOp::EffectRemove, host);
				self.params.b = effect.raw();
			}
			EditCommand::EffectMove {
				host,
				effect,
				new_index,
			} => {
				self.simple(QuercusEditOp::EffectMove, host);
				self.params.b = effect.raw();
				self.params.track_index = new_index;
			}
			EditCommand::EffectSetEnabled { effect, enabled } => {
				self.simple(QuercusEditOp::EffectSetEnabled, effect);
				self.params.enabled = enabled;
			}
			EditCommand::SetNodeStandardValue {
				node,
				input_id,
				element,
				value,
			} => {
				self.simple(QuercusEditOp::SetNodeStandardValue, node);
				self.input_id(input_id);
				self.params.element = element;
				self.params.node_value = value;
			}
			EditCommand::UpsertNodeKeyframe {
				node,
				input_id,
				element,
				time,
				value,
				interpolation,
				bezier_in,
				bezier_out,
			} => {
				self.simple(QuercusEditOp::UpsertNodeKeyframe, node);
				self.input_id(input_id);
				self.params.element = element;
				self.params.rate = time;
				self.params.node_value = value;
				self.params.kind = match interpolation {
					KeyframeInterpolation::Hold => 0,
					KeyframeInterpolation::Linear => 1,
					KeyframeInterpolation::Bezier => 2,
				};
				(self.params.bezier_in_x, self.params.bezier_in_y) = bezier_in;
				(self.params.bezier_out_x, self.params.bezier_out_y) = bezier_out;
			}
			EditCommand::RemoveNodeKeyframe {
				node,
				input_id,
				element,
				time,
			} => {
				self.simple(QuercusEditOp::RemoveNodeKeyframe, node);
				self.input_id(input_id);
				self.params.element = element;
				self.params.rate = time;
			}
			EditCommand::SetNodeValueAtTime {
				node,
				input_id,
				element,
				time,
				value,
			} => {
				self.simple(QuercusEditOp::SetNodeValueAtTime, node);
				self.input_id(input_id);
				self.params.element = element;
				self.params.rate = time;
				self.params.node_value = value;
			}
			EditCommand::PushNodeButton { node, input_id } => {
				self.simple(QuercusEditOp::PushNodeButton, node);
				self.input_id(input_id);
			}
			EditCommand::SetNodeTextValue {
				node,
				input_id,
				element,
				value,
			} => {
				self.simple(QuercusEditOp::SetNodeTextValue, node);
				self.input_id(input_id);
				self.params.element = element;
				self.name(value);
			}
			EditCommand::ReplaceFootage { footage, path } => {
				self.simple(QuercusEditOp::ReplaceFootage, footage);
				self.path(path);
			}
			EditCommand::DeleteEntry { node } => self.simple(QuercusEditOp::DeleteEntry, node),
			EditCommand::SetFootageProxyParams { footage, params } => {
				self.simple(QuercusEditOp::SetFootageProxyParams, footage);
				self.params.proxy_params = params;
			}
			EditCommand::ClearFootageProxyParams { footage } => {
				self.simple(QuercusEditOp::ClearFootageProxyParams, footage);
			}
			EditCommand::CreateSequenceFromFootage { footage, name } => {
				self.simple(QuercusEditOp::CreateSequenceFromFootage, footage);
				self.name(name);
			}
			EditCommand::DeleteProxy { footage } => self.simple(QuercusEditOp::DeleteProxy, footage),
			EditCommand::SetProxyEnabled { footage, enabled } => {
				self.simple(QuercusEditOp::SetProxyEnabled, footage);
				self.params.enabled = enabled;
			}
			EditCommand::SetMulticamEnabled { clips, enabled } => {
				self.op = QuercusEditOp::SetMulticamEnabled;
				self.ids(clips);
				self.params.enabled = enabled;
			}
			EditCommand::SwitchMulticam {
				clip,
				source,
				split_clip,
				playhead,
			} => {
				self.simple(QuercusEditOp::SwitchMulticam, clip);
				self.params.kind = source;
				self.params.enabled = split_clip;
				self.params.rate = playhead;
			}
			EditCommand::CreateMulticamSequence {
				host_sequence,
				angles,
				offsets,
				name,
			} => {
				self.simple(QuercusEditOp::CreateMulticamSequence, host_sequence);
				self.ids(angles);
				self.values(offsets);
				self.name(name);
			}
		}
	}
}
