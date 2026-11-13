//! Sequence/footage montage construction shared by inline and worker renders.

use quercus_core::Rational;
use quercus_node::block::{AdjustmentBlockBehavior, ClipBlockBehavior, clip_input};
use quercus_node::footage::FootageBehavior;
use quercus_node::id::NodeId;
use quercus_node::nodes::multicamnode::{CURRENT_INPUT, SEQUENCE_INPUT, SEQUENCE_TYPE_INPUT};
use quercus_node::project::Project;
use quercus_node::sequence::SequenceBehavior;
use quercus_node::track::{TrackBehavior, TrackListBehavior, TrackType};
use quercus_node::value::ValueType;
use quercus_render::ticket::{AdjustmentSpan, MontageAudio, MontageClip, MontageEffect};

use crate::editor_commands as ops;

/// Returns preview video media after proxy and source-stream selection.
pub(crate) fn preview_media(footage: &FootageBehavior) -> Option<(String, i32)> {
	let first = footage.streams.iter().find(|stream| stream.is_video);
	let original = first
		.map(|stream| (footage.filename.clone(), stream.index))
		.or_else(|| (!footage.filename.is_empty()).then(|| (footage.filename.clone(), 0)))?;
	let use_proxy =
		quercus_core::configstore::ConfigStore::instance().get_bool(None, "UseProxyMedia", 1) != 0;
	if !use_proxy || !footage.proxy_enabled || footage.proxy.is_empty() {
		return Some(original);
	}
	if quercus_codec::proxymanager::ProxyManager::get_proxy_state(&footage.proxy)
		!= quercus_codec::proxymanager::ProxyState::Ready
	{
		return Some(original);
	}
	match first {
		Some(stream) if footage.proxy_video_stream_index == stream.index => {
			Some((footage.proxy.clone(), 0))
		}
		_ => Some(original),
	}
}

/// Collect the effect descriptors applied to `clip` at `time`.
///
/// `project` supplies the graph, `clip` is the clip node, and `time` is the
/// timeline position used to evaluate each effect input's standard/keyframed
/// value. Returns one [`MontageEffect`] per effect in the clip's chain that has
/// an effect input; texture/samples/matrix, hidden inputs and `enabled_in` are
/// excluded from `params`, while `enabled` records the effect's own flag.
fn clip_effects(project: &Project, clip: NodeId, time: Rational) -> Vec<MontageEffect> {
	crate::effect::chain(&project.graph, clip)
		.into_iter()
		.filter(|&effect| crate::effect::effect_input_of(&project.graph, effect).is_some())
		.filter_map(|effect| {
			let entry = project.graph.get(effect)?;
			let params = entry
				.core
				.inputs
				.iter()
				.filter(|input| {
					!matches!(
						input.value_type,
						ValueType::Texture | ValueType::Samples | ValueType::Matrix
					) && input.flags & quercus_node::input::flags::HIDDEN == 0
						&& input.id != quercus_node::node::ENABLED_INPUT
				})
				.map(|input| (input.id.clone(), entry.core.value_at_time(&input.id, -1, time)))
				.collect();
			Some(MontageEffect {
				type_id: entry.behavior.type_id().to_string(),
				enabled: crate::effect::is_enabled(&project.graph, effect),
				effect_input_id: crate::effect::effect_input_of(&project.graph, effect),
				params,
			})
		})
		.collect()
}

/// Find the enabled adjustment block covering `time` on one track.
///
/// `project` supplies the graph, `blocks` is the track's block list in order,
/// `time` is the timeline position, and `below` is the track index stored in the
/// returned span. Returns the first [`AdjustmentSpan`] whose block is enabled
/// and whose `[in, out)` range covers `time` (with its effects evaluated at
/// `time`), or `None` when no adjustment block covers it.
fn covering_adjustment(
	project: &Project,
	blocks: &[NodeId],
	time: Rational,
	below: usize,
) -> Option<AdjustmentSpan> {
	for &block in blocks {
		let Some(adjustment) = project
			.graph
			.get(block)
			.and_then(|entry| entry.behavior.as_any())
			.and_then(|behavior| behavior.downcast_ref::<AdjustmentBlockBehavior>())
		else {
			continue;
		};
		if adjustment.core.enabled && time >= adjustment.core.in_() && time < adjustment.core.out()
		{
			return Some(AdjustmentSpan {
				in_time: adjustment.core.in_(),
				out_time: adjustment.core.out(),
				track_index: below,
				effects: clip_effects(project, block, time),
			});
		}
	}
	None
}

/// Resolve the active multicam angle's media for `clip` at `time`.
///
/// `project` supplies the graph; `clip` is the timeline clip; `host_in` is the
/// host clip's timeline in point, used to offset into the angle's media; `time`
/// is the current timeline position. Returns `(filename, stream_index,
/// media_in)` for the angle block covering `time`, or `None` when `clip` is not
/// fed by a multicam node, the current angle has no media at `time`, or the
/// referenced sequence/track/block is missing.
fn multicam_media(
	project: &Project,
	clip: NodeId,
	host_in: Rational,
	time: Rational,
) -> Option<(String, i32, Rational)> {
	let multicam = project
		.graph
		.connected_output(clip, clip_input::TEXTURE_INPUT, -1)?;
	let entry = project.graph.get(multicam)?;
	if entry.behavior.type_id() != "org.olivevideoeditor.Olive.multicam" {
		return None;
	}
	let source = entry.core.standard_value(CURRENT_INPUT, -1).to_double() as i32;
	if source < 0 {
		return None;
	}
	let sequence = project
		.graph
		.connected_output(multicam, SEQUENCE_INPUT, -1)?;
	let kind = entry
		.core
		.standard_value(SEQUENCE_TYPE_INPUT, -1)
		.to_double() as i32;
	let kind = TrackType::from_c(kind).unwrap_or(TrackType::Video);
	let list = ops::track_list_of(&project.graph, sequence, kind)?;
	let list = ops::track_list_behavior(&project.graph, list)?;
	let track = *list.tracks.get(source as usize)?;
	let track = ops::track_behavior(&project.graph, track)?;
	track.blocks.iter().find_map(|&block| {
		let clip = ops::clip_behavior(&project.graph, block)?;
		if time < clip.core.in_() || time >= clip.core.out() {
			return None;
		}
		let footage_id = ops::find_input_footage(&project.graph, block)?;
		let footage = ops::footage_behavior(&project.graph, footage_id)?;
		let (filename, stream_index) = preview_media(footage)?;
		let media_in = clip.core.media_in + (host_in - clip.core.in_());
		Some((filename, stream_index, media_in))
	})
}

/// Build the bottom-to-top video montage and adjustment spans for `time`.
///
/// `project` supplies the graph, `sequence` is the sequence node, and `time` is
/// the timeline position. Returns `(clips, adjustments)`: one [`MontageClip`]
/// per visible video clip across the sequence's video tracks (ordered
/// bottom-to-top) and one [`AdjustmentSpan`] per enabled adjustment block
/// covering `time` (its `track_index` records the number of clips below it).
/// Both vectors are empty when `sequence` is stale or not a sequence node.
pub(crate) fn video_montage_with_adjustments(
	project: &Project,
	sequence: NodeId,
	time: Rational,
) -> (Vec<MontageClip>, Vec<AdjustmentSpan>) {
	let mut clips = Vec::new();
	let mut adjustments = Vec::new();
	let Some(sequence) = project
		.graph
		.get(sequence)
		.and_then(|entry| entry.behavior.as_any())
		.and_then(|behavior| behavior.downcast_ref::<SequenceBehavior>())
	else {
		return (clips, adjustments);
	};
	for list_id in &sequence.track_lists {
		let Some(list) = project
			.graph
			.get(*list_id)
			.and_then(|entry| entry.behavior.as_any())
			.and_then(|behavior| behavior.downcast_ref::<TrackListBehavior>())
		else {
			continue;
		};
		if list.kind != TrackType::Video {
			continue;
		}
		for track_id in &list.tracks {
			let Some(track) = project
				.graph
				.get(*track_id)
				.and_then(|entry| entry.behavior.as_any())
				.and_then(|behavior| behavior.downcast_ref::<TrackBehavior>())
			else {
				continue;
			};
			if track.muted {
				continue;
			}
			if let Some(span) = covering_adjustment(project, &track.blocks, time, clips.len()) {
				adjustments.push(span);
				continue;
			}
			for block_id in &track.blocks {
				let Some(clip) = project
					.graph
					.get(*block_id)
					.and_then(|entry| entry.behavior.as_any())
					.and_then(|behavior| behavior.downcast_ref::<ClipBlockBehavior>())
				else {
					continue;
				};
				let in_time = clip.core.in_();
				let out_time = clip.core.out();
				if time < in_time || time >= out_time {
					continue;
				}
				let is_multicam = project
					.graph
					.connected_output(*block_id, clip_input::TEXTURE_INPUT, -1)
					.and_then(|source| project.graph.get(source))
					.is_some_and(|entry| {
						entry.behavior.type_id() == "org.olivevideoeditor.Olive.multicam"
					});
				let (filename, stream_index, media_in) =
					match multicam_media(project, *block_id, in_time, time) {
						Some(media) => media,
						// A multicam clip whose current angle has no media at
						// this time must stay transparent: falling back to any
						// footage reachable through the graph would show the
						// wrong angle.
						None if is_multicam => continue,
						None => {
							let Some(footage_id) =
								ops::find_input_footage(&project.graph, *block_id)
							else {
								continue;
							};
							let Some(footage) = ops::footage_behavior(&project.graph, footage_id)
							else {
								continue;
							};
							let Some((filename, stream_index)) = preview_media(footage) else {
								continue;
							};
							(filename, stream_index, clip.core.media_in)
						}
					};
				let audio = MontageAudio::from_block_core(&clip.core);
				clips.push(MontageClip {
					filename,
					stream_index,
					in_time,
					out_time,
					media_in,
					gain: audio.gain,
					pan: audio.pan,
					fade_in: audio.fade_in,
					fade_out: audio.fade_out,
					effects: clip_effects(project, *block_id, time),
					audio_effects: audio.effects,
				});
			}
		}
	}
	(clips, adjustments)
}

/// Build a montage containing only video track `track` of `sequence`.
///
/// `project` supplies the graph; `sequence` and `track` are node ids; `time` is
/// the timeline position. Returns an empty vector when the sequence is stale,
/// `track` is not a video track belonging to the sequence, the track is muted,
/// or no clip covers `time`. Used for multicam angle previews.
pub(crate) fn single_track_video_montage(
	project: &Project,
	sequence: NodeId,
	track: NodeId,
	time: Rational,
) -> Vec<MontageClip> {
	let mut clips = Vec::new();
	let Some(sequence) = project
		.graph
		.get(sequence)
		.and_then(|entry| entry.behavior.as_any())
		.and_then(|behavior| behavior.downcast_ref::<SequenceBehavior>())
	else {
		return clips;
	};
	let belongs_to_sequence = sequence.track_lists.iter().any(|list_id| {
		project
			.graph
			.get(*list_id)
			.and_then(|entry| entry.behavior.as_any())
			.and_then(|behavior| behavior.downcast_ref::<TrackListBehavior>())
			.is_some_and(|list| list.kind == TrackType::Video && list.tracks.contains(&track))
	});
	if !belongs_to_sequence {
		return clips;
	}
	let Some(track) = project
		.graph
		.get(track)
		.and_then(|entry| entry.behavior.as_any())
		.and_then(|behavior| behavior.downcast_ref::<TrackBehavior>())
	else {
		return clips;
	};
	if track.muted {
		return clips;
	}
	for block_id in &track.blocks {
		let Some(clip) = project
			.graph
			.get(*block_id)
			.and_then(|entry| entry.behavior.as_any())
			.and_then(|behavior| behavior.downcast_ref::<ClipBlockBehavior>())
		else {
			continue;
		};
		let in_time = clip.core.in_();
		let out_time = clip.core.out();
		if time < in_time || time >= out_time {
			continue;
		}
		let Some(footage_id) = ops::find_input_footage(&project.graph, *block_id) else {
			continue;
		};
		let Some(footage) = ops::footage_behavior(&project.graph, footage_id) else {
			continue;
		};
		let Some((filename, stream_index)) = preview_media(footage) else {
			continue;
		};
		let audio = MontageAudio::from_block_core(&clip.core);
		clips.push(MontageClip {
			filename,
			stream_index,
			in_time,
			out_time,
			media_in: clip.core.media_in,
			gain: audio.gain,
			pan: audio.pan,
			fade_in: audio.fade_in,
			fade_out: audio.fade_out,
			effects: clip_effects(project, *block_id, time),
			audio_effects: audio.effects,
		});
	}
	clips
}

/// Native pixel resolution of a footage's proxy media.
///
/// `footage` supplies the custom proxy parameters (falling back to the global
/// proxy config) and the source video stream. Returns `(width, height)` from the
/// proxy params when positive, otherwise the source dimensions divided by
/// `divider` (rounded down, minimum 2); `None` when there is no video stream.
fn proxy_resolution(footage: &FootageBehavior) -> Option<(i32, i32)> {
	let params = footage
		.custom_proxy_params
		.clone()
		.unwrap_or_else(|| quercus_codec::proxymanager::ProxyManager::proxy_params_from_config());
	if params.width > 0 && params.height > 0 {
		return Some((params.width, params.height));
	}
	let source = footage
		.streams
		.iter()
		.find(|stream| stream.is_video)?
		.video
		.as_ref()?;
	let divider = params.divider.max(1) as u32;
	Some((
		(source.width as u32 / divider).max(2) as i32,
		(source.height as u32 / divider).max(2) as i32,
	))
}

/// Clamp a render size to the native resolution of active proxies.
///
/// `project`, `sequence` and `time` select the visible clips; `width`/`height`
/// are the requested render dimensions. For every visible clip currently using
/// its proxy, the result is scaled down by the smallest proxy/requested ratio
/// (preserving aspect ratio), rounded and floored at 2 px; if no proxy is active
/// (or the scale is >= 1) the input size is returned unchanged. Only video
/// tracks are considered.
pub(crate) fn clamp_render_size(
	project: &Project,
	sequence: NodeId,
	time: Rational,
	width: i32,
	height: i32,
) -> (i32, i32) {
	let Some(sequence) = project
		.graph
		.get(sequence)
		.and_then(|entry| entry.behavior.as_any())
		.and_then(|behavior| behavior.downcast_ref::<SequenceBehavior>())
	else {
		return (width, height);
	};
	let mut limits = Vec::new();
	for list_id in &sequence.track_lists {
		let Some(list) = project
			.graph
			.get(*list_id)
			.and_then(|entry| entry.behavior.as_any())
			.and_then(|behavior| behavior.downcast_ref::<TrackListBehavior>())
		else {
			continue;
		};
		if list.kind != TrackType::Video {
			continue;
		}
		for track_id in &list.tracks {
			let Some(track) = project
				.graph
				.get(*track_id)
				.and_then(|entry| entry.behavior.as_any())
				.and_then(|behavior| behavior.downcast_ref::<TrackBehavior>())
			else {
				continue;
			};
			for block_id in &track.blocks {
				let Some(clip) = project
					.graph
					.get(*block_id)
					.and_then(|entry| entry.behavior.as_any())
					.and_then(|behavior| behavior.downcast_ref::<ClipBlockBehavior>())
				else {
					continue;
				};
				if time < clip.core.in_() || time >= clip.core.out() {
					continue;
				}
				let Some(footage_id) = ops::find_input_footage(&project.graph, *block_id) else {
					continue;
				};
				let Some(footage) = ops::footage_behavior(&project.graph, footage_id) else {
					continue;
				};
				if preview_media(footage).map(|media| media.0) != Some(footage.proxy.clone()) {
					continue;
				}
				if let Some(limit) = proxy_resolution(footage) {
					limits.push(limit);
				}
			}
		}
	}
	let mut scale = 1.0f64;
	for (limit_width, limit_height) in limits {
		if limit_width > 0 && limit_height > 0 {
			scale = scale
				.min(limit_width as f64 / width as f64)
				.min(limit_height as f64 / height as f64);
		}
	}
	if scale >= 1.0 {
		(width, height)
	} else {
		(
			((width as f64 * scale).round() as i32).max(2),
			((height as f64 * scale).round() as i32).max(2),
		)
	}
}
