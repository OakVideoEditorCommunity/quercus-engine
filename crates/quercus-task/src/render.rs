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

//! `RenderTask` abstract base, mirroring `src/task/src/render/render.h`.
//!
//! The abstract parent of [`crate::export::ExportTask`] and
//! [`crate::precache::PreCacheTask`]. It renders frames/audio through the
//! direct `quercus_render::ticket::TicketArena` (single-lib unification: the
//! CHandle-based ticket C ABI and `crate::stubs` are gone) and hands each
//! result to a virtual hook that the subclass implements via the
//! [`RenderTaskBehavior`] trait.
//!
//! The viewer node is now a [`crate::nodeops::NodeRef`] (project + node
//! id): footage viewers render through `VideoTicketParams::footage`
//! (single-stream decode), sequence viewers are flattened into an ordered
//! clip montage (`VideoTicketParams::montage`) resolved from the
//! sequence's track lists. Tickets run on the process-wide
//! `quercus_render::manager::RenderManager` arena when the manager is
//! initialized, otherwise on a private process dispatcher + arena owned
//! by this render run (M15 S2: the in-process thread pool is gone).
//!
//! ## Concurrent render loop
//!
//! Like the C++ original (`render.cpp`), the loop keeps up to
//! `max_inflight` tickets running at once (default
//! `std::thread::available_parallelism()`). Tickets report completion
//! through the arena's boxed completion callback (fired on the ticket's
//! own finishing thread), which pushes the finished ticket into a shared
//! queue and wakes the render thread (queue + condvar, exactly the C++
//! design). Tickets may complete out of order; a **reorder buffer**
//! delivers the results to the hooks in timestamp order — the audio range
//! first (the C++ queue-audio-first order), then every frame in ascending
//! time — so the observable per-frame contract (`frame_downloaded`/
//! `audio_downloaded` in order, progress updates, cancellation between
//! frames) is unchanged.
//!
//! CPP-PARITY: src/task/src/render/render.h

use quercus_codec::proxymanager;
use quercus_core::configstore;
use quercus_core::PixelFormat;
use quercus_node::graph;
use quercus_node::id;
use quercus_node::input;
use quercus_node::node;
use quercus_node::project;
use quercus_node::value;
use quercus_render::audio;
use quercus_render::eval;
use quercus_render::manager;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time;

use quercus_core::texture::Texture;
use quercus_core::videoparams::VideoParams;
use quercus_node::footage::FootageBehavior;
use quercus_node::sequence::SequenceBehavior;
use quercus_node::track::{TrackBehavior, TrackListBehavior, TrackType};
use quercus_render::procpool::{bgra8_to_f32_rgba, ShmFrameRef};
use quercus_render::ticket::{
	ticket_kind, AudioTicketParams, MontageAudio, MontageClip, MontageEffect, TicketArena,
	TicketId, TicketPayload, TicketResult, VideoTicketParams,
};
use quercus_render::worker::{InlineDispatcher, JobDispatch};

use crate::error::{Error, Result};
use crate::nodeops::{find_input_footage, pixel_format_from_code, NodeRef, ProjectRef};
use crate::task::Task;
use quercus_core::{Rational, TimeRange};

/// `quercus_render::ticket::ticket_kind::VIDEO` re-export (loop-local kind).
const TICKET_VIDEO: i32 = ticket_kind::VIDEO;
/// `quercus_render::ticket::ticket_kind::AUDIO` re-export (loop-local kind).
const TICKET_AUDIO: i32 = ticket_kind::AUDIO;
/// Delivery slot for a frame past the picture's end (filled black).
const TICKET_BLACK: i32 = -1;

/// Overrides applied to a render ticket, mirroring the C++ `ForceParams`
/// struct in render.h (fields map onto `quercusrender_video_ticket_params`).
///
/// CPP-PARITY: src/task/src/render/render.h (ForceParams)
///
/// The color-matrix and color-output fields of the C ABI params were
/// dropped with the C ABI: the direct ticket arena carries a forced size
/// and pixel format only (the eval producer performs no color
/// management). The matrix fields are retained for API parity but unused.
#[derive(Clone, Debug)]
pub struct ForceParams {
	/// Forced output width (0 = off).
	pub force_width: i32,
	/// Forced output height (0 = off).
	pub force_height: i32,
	/// Forced color matrix (row-major 4x4); retained for C++ API parity,
	/// not applied by the direct ticket arena.
	pub force_matrix: [f64; 16],
	/// Whether `force_matrix` is in effect; retained for parity, unused.
	pub has_force_matrix: bool,
	/// Forced pixel format (`quercus_core::PixelFormat` as int; -1 = off).
	pub force_format: i32,
	/// Forced channel count (0 = off).
	pub force_channel_count: i32,
}

impl Default for ForceParams {
	/// All overrides off: in particular `force_format` is `-1` ("off"), not
	/// `0` (which is a real `PixelFormat` code — U8, and the F32 render
	/// pipeline rejects it).
	fn default() -> Self {
		ForceParams {
			force_width: 0,
			force_height: 0,
			force_matrix: [0.0; 16],
			has_force_matrix: false,
			force_format: -1,
			force_channel_count: 0,
		}
	}
}

/// Subclass hooks, standing in for the C++ protected virtuals
/// `download_frame`/`frame_downloaded`/`audio_downloaded`/`encode_subtitle`.
/// Frames and audio are the direct `quercusrender` value types (the deleted
/// C ABI handed `CHandle`s; single-lib unification delivers the payload
/// values themselves).
pub trait RenderTaskBehavior {
	/// Called for each rendered video frame.
	fn frame_downloaded(
		&mut self,
		task: &mut Task,
		frame: &quercus_core::texture::Texture,
	) -> Result<()>;
	/// Called for each rendered audio buffer.
	fn audio_downloaded(
		&mut self,
		task: &mut Task,
		samples: &quercus_render::ticket::AudioSamples,
	) -> Result<()>;
	/// Called to encode a subtitle.
	fn encode_subtitle(&mut self, task: &mut Task, text: &str) -> Result<()>;
}

/// The abstract render task base. Holds the shared [`Task`], the sequence
/// output params, the viewer node, and the subclass behavior.
pub struct RenderTask {
	/// The shared task base.
	pub base: Task,
	/// Output video params (value type; `None` = invalid/unavailable).
	pub video_params: Option<VideoParams>,
	/// The node being rendered (footage or sequence, see the module
	/// docs).
	pub viewer: NodeRef,
	/// Force params applied to every ticket.
	pub force_params: ForceParams,
	/// The subclass behavior.
	pub behavior: Option<Box<dyn RenderTaskBehavior + Send>>,
	// --- private render inputs (set by the subclass before render()) ---
	/// `olive::RenderMode::Mode` as int.
	mode: i32,
	/// Whether audio ranges are rendered.
	audio_enabled: bool,
	/// Whether video frames are rendered.
	video_enabled: bool,
	/// End of the picture; frames at/after this time are delivered black
	/// (video shorter than the audio/export range).
	video_end: Option<Rational>,
	/// The range rendered by [`RenderTask::render`].
	export_range: TimeRange,
	/// Whether the render loop emits progress itself (the export task
	/// disables it and reports progress per written frame).
	native_progress_signalling: bool,
	/// Total number of frames in `export_range` (computed by `render`).
	total_frames: i64,
	/// Maximum render tickets kept in flight at once (the C++
	/// `maximum_rendered_frames`; defaults to `available_parallelism`).
	max_inflight: usize,
}

impl RenderTask {
	/// Create a render task for `viewer` (a footage or sequence node of
	/// `viewer.0`) with default (empty) render inputs.
	pub fn new(
		base: Task,
		video_params: Option<VideoParams>,
		viewer: NodeRef,
		force_params: ForceParams,
		behavior: Option<Box<dyn RenderTaskBehavior + Send>>,
	) -> RenderTask {
		RenderTask {
			base,
			video_params,
			viewer,
			force_params,
			behavior,
			mode: 0,
			audio_enabled: false,
			video_enabled: true,
			video_end: None,
			export_range: TimeRange::new(Rational::new(0, 1), Rational::new(0, 1)),
			native_progress_signalling: true,
			total_frames: 0,
			max_inflight: thread::available_parallelism()
				.map(|n| n.get())
				.unwrap_or(1)
				.max(1),
		}
	}

	/// Set the three inputs the render loop reads, normally the last call
	/// each concrete subclass's `run` makes before [`RenderTask::render`]
	/// (the export and pre-cache tasks both pass mode `0`).
	///
	/// `mode` is the C++ `olive::RenderMode::Mode` value as an `i32`
	/// (`0` = `k_online`), carried through for render-mode-dependent
	/// behavior. `audio_enabled` gates submission of the single audio ticket
	/// (a video-only export passes `false`); video submission is gated
	/// separately by [`RenderTask::set_video_enabled`]. `export_range` is the
	/// half-open range the loop walks, and also the range handed to the audio
	/// ticket. The color-manager / frame-cache handle arguments of the
	/// deleted C ABI path are gone: the direct ticket arena has no color
	/// management (the eval producer ignores it) and the frame cache is keyed
	/// by the viewer node identity for pre-cache runs.
	///
	/// [`RenderTask::new`] starts with `mode = 0`, audio disabled, and the
	/// empty range `(0/1, 0/1)`, so every caller must set these before
	/// rendering anything meaningful.
	pub fn set_render_inputs(&mut self, mode: i32, audio_enabled: bool, export_range: TimeRange) {
		self.mode = mode;
		self.audio_enabled = audio_enabled;
		self.export_range = export_range;
	}

	/// Enable/disable video tickets (a WAV/audio-only export disables them).
	pub fn set_video_enabled(&mut self, enabled: bool) {
		self.video_enabled = enabled;
	}

	/// Fill frames at/after `end` with black instead of requesting a video
	/// ticket (video shorter than the export range).
	pub fn set_video_end(&mut self, end: Option<Rational>) {
		self.video_end = end;
	}

	/// Enable or disable the render loop's own progress signalling.
	///
	/// When enabled (the default set by [`RenderTask::new`]), every delivered
	/// frame — a video ticket result or a black pad — increments the loop's
	/// counter and calls `Task::emit_progress(counter / total)`, so the task
	/// reports monotonically as frames are handed to the behavior. Audio
	/// deliveries do not signal. Subclasses that report progress themselves
	/// (the export task counts written frames) switch this off to avoid two
	/// progress sources; [`RenderTask::total_frames`] is still maintained.
	pub fn set_native_progress_signalling(&mut self, enabled: bool) {
		self.native_progress_signalling = enabled;
	}

	/// Override the number of render tickets kept in flight at once.
	///
	/// Defaults to `std::thread::available_parallelism()`, mirroring the
	/// C++ `std::max(1, int(std::thread::hardware_concurrency()))`. Primarily
	/// a test hook: a smaller window makes concurrent-completion tests
	/// deterministic regardless of the host core count.
	pub fn set_max_inflight(&mut self, max: usize) {
		self.max_inflight = max.max(1);
	}

	/// The number of frames computed by the last [`RenderTask::render`] run.
	pub fn total_frames(&self) -> i64 {
		self.total_frames
	}

	/// Frame duration rational from the video params (falls back to 1/1
	/// when the params are absent/invalid).
	fn timebase(&self) -> Rational {
		let Some(params) = &self.video_params else {
			return Rational::new(1, 1);
		};
		let (num, den) = params.frame_rate_as_time_base();
		if num <= 0 || den <= 0 {
			Rational::new(1, 1)
		} else {
			Rational::new(num as i64, den as i64)
		}
	}

	/// Allocate a zeroed CPU frame standing in for the frame at `time`.
	///
	/// Used when the picture ends before the export/audio range
	/// ([`RenderTask::set_video_end`]): the slot is delivered as black
	/// instead of via a ticket. The size is the forced output size when
	/// [`ForceParams`] sets one, else the video params' size; the frame is
	/// four-channel, [`quercus_core::PixelFormat::F32`], stamped with `time`, and
	/// zero-filled by [`quercus_core::texture::Frame::allocate`].
	///
	/// Returns `None` when no size is known (no force size and no video
	/// params), when the size is non-positive, or when the allocation fails
	/// — the render loop turns that into
	/// `Error::Failed("Could not allocate a black pad frame")`.
	fn black_texture(&self, time: Rational) -> Option<Texture> {
		let (width, height) = self.force_size().or_else(|| {
			self.video_params
				.as_ref()
				.map(|params| (params.width, params.height))
		})?;
		if width <= 0 || height <= 0 {
			return None;
		}
		let mut frame = quercus_core::texture::Frame::new();
		frame.width = width;
		frame.height = height;
		frame.channels = 4;
		frame.format = PixelFormat::F32;
		frame.timestamp = time;
		if !frame.allocate() {
			return None;
		}
		Some(Texture::Cpu(frame))
	}

	/// The forced output size from [`ForceParams`], or `None`.
	fn force_size(&self) -> Option<(i32, i32)> {
		if self.force_params.force_width > 0 && self.force_params.force_height > 0 {
			Some((
				self.force_params.force_width,
				self.force_params.force_height,
			))
		} else {
			None
		}
	}

	/// The forced pixel format from [`ForceParams`], or `None`.
	fn force_format(&self) -> Option<quercus_core::PixelFormat> {
		if self.force_params.force_format >= 0 {
			Some(pixel_format_from_code(self.force_params.force_format))
		} else {
			None
		}
	}

	/// The effect stack of a clip block as montage effect descriptors
	/// (source-first order). Mirrors the app's `effectchain` walk —
	/// quercus-task cannot link quercus-app, so the export path keeps its own
	/// copy over the public quercusnode graph API. Disabled effects ride
	/// along with `enabled = false`; the worker bypasses them (the C++
	/// traverser's bypass pushes the effect input through unchanged).
	/// Parameters are the non-hidden, non-connection data inputs at
	/// their standard (non-keyframed) values.
	fn clip_effects(graph: &graph::Graph, host: id::NodeId, time: Rational) -> Vec<MontageEffect> {
		// Walk the chain: from the host's effect input upstream until an
		// unconnected input or a node without an effect input; a `seen`
		// guard protects against malformed cycles.
		let mut chain = Vec::new();
		let mut cur = host;
		let mut seen: Vec<id::NodeId> = Vec::new();
		loop {
			if seen.contains(&cur) {
				break;
			}
			seen.push(cur);
			let Some(entry) = graph.get(cur) else {
				break;
			};
			let input = &entry.core.effect_input;
			if input.is_empty() {
				break;
			}
			let Some(up) = graph.connected_output(cur, input, -1) else {
				break;
			};
			chain.push(up);
			cur = up;
		}
		chain.reverse();
		chain
			.into_iter()
			// The walk runs all the way to the media source; drop the
			// source node (the one WITHOUT an effect input) — the montage
			// decodes the footage itself.
			.filter(|&fx| {
				graph
					.get(fx)
					.map(|e| !e.core.effect_input.is_empty())
					.unwrap_or(false)
			})
			.filter_map(|fx| {
				let entry = graph.get(fx)?;
				let enabled = matches!(
					entry.core.standard_value(node::ENABLED_INPUT, -1),
					value::NodeValue::Boolean(true)
				);
				let effect_input_id = if entry.core.effect_input.is_empty() {
					None
				} else {
					Some(entry.core.effect_input.clone())
				};
				let mut params = Vec::new();
				for input in &entry.core.inputs {
					if matches!(
						input.value_type,
						value::ValueType::Texture
							| value::ValueType::Samples
							| value::ValueType::Matrix
					) {
						continue;
					}
					if input.flags & input::flags::HIDDEN != 0 {
						continue;
					}
					if input.id == node::ENABLED_INPUT {
						continue;
					}
					params.push((
						input.id.clone(),
						entry.core.value_at_time(&input.id, -1, time),
					));
				}
				Some(MontageEffect {
					type_id: entry.behavior.type_id().to_string(),
					enabled,
					effect_input_id,
					params,
				})
			})
			.collect()
	}

	/// Flatten the video tracks of `sequence` into an ordered montage
	/// (bottom-most track first so the topmost track — the highest-numbered
	/// one, the list's last — composites last;
	/// `// CPP-PARITY: M12 P0 montage contract`).
	fn video_montage(
		project: &ProjectRef,
		sequence: id::NodeId,
		time: Rational,
	) -> Vec<MontageClip> {
		let guard = project.lock().unwrap_or_else(|e| e.into_inner());
		let Some(entry) = guard.graph.get(sequence) else {
			return Vec::new();
		};
		let Some(seq) = entry
			.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<SequenceBehavior>())
		else {
			return Vec::new();
		};
		let mut montage = Vec::new();
		for list_id in seq.track_lists.iter().filter(|i| i.valid()) {
			let Some(le) = guard.graph.get(*list_id) else {
				continue;
			};
			let Some(list) = le
				.behavior
				.as_any()
				.and_then(|a| a.downcast_ref::<TrackListBehavior>())
			else {
				continue;
			};
			if list.kind != TrackType::Video {
				continue;
			}
			for track_id in list.tracks.iter() {
				let Some(te) = guard.graph.get(*track_id) else {
					continue;
				};
				let Some(track) = te
					.behavior
					.as_any()
					.and_then(|a| a.downcast_ref::<TrackBehavior>())
				else {
					continue;
				};
				if track.muted {
					continue;
				}
				for block_id in &track.blocks {
					let Some(core) = crate::nodeops::block_core_of(&guard.graph, *block_id) else {
						continue;
					};
					if time < core.in_() || time >= core.out() {
						continue;
					}
					let Some(footage_id) = find_input_footage(&guard.graph, *block_id) else {
						continue;
					};
					let Some(fe) = guard.graph.get(footage_id) else {
						continue;
					};
					let Some(footage) = fe
						.behavior
						.as_any()
						.and_then(|a| a.downcast_ref::<FootageBehavior>())
					else {
						continue;
					};
					let audio = MontageAudio::from_block_core(&core);
					montage.push(MontageClip {
						filename: footage.filename.clone(),
						stream_index: 0,
						in_time: core.in_(),
						out_time: core.out(),
						media_in: core.media_in,
						gain: audio.gain,
						pan: audio.pan,
						fade_in: audio.fade_in,
						fade_out: audio.fade_out,
						effects: Self::clip_effects(&guard.graph, *block_id, time),
						audio_effects: audio.effects,
					});
				}
			}
		}
		montage
	}

	/// Pick a footage's audio stream (proxy-aware) for the export montage.
	/// This mirrors the preview path: the first non-video stream, not
	/// stream 0 (which is video in most containers).
	fn audio_media(footage: &FootageBehavior) -> Option<(String, i32)> {
		let original = footage
			.streams
			.iter()
			.find(|stream| !stream.is_video)
			.map(|stream| (footage.filename.clone(), stream.index))
			.or_else(|| (!footage.filename.is_empty()).then(|| (footage.filename.clone(), 1)))?;
		let use_proxy =
			configstore::ConfigStore::instance().get_bool(None, "UseProxyMedia", 1) != 0;
		if use_proxy
			&& footage.proxy_enabled
			&& proxymanager::ProxyManager::get_proxy_state(&footage.proxy)
				== proxymanager::ProxyState::Ready
			&& proxymanager::ProxyManager::proxy_filename_has_audio(&footage.proxy)
		{
			Some((footage.proxy.clone(), 1))
		} else {
			Some(original)
		}
	}

	/// Resolve the audible clips of `sequence` at `time` into montage
	/// entries for the audio mixer.
	///
	/// Walks `sequence`'s track lists, keeps only `TrackType::Audio` lists,
	/// skips muted tracks, and emits one [`MontageClip`] per block whose
	/// half-open `[in, out)` range contains `time`. Each clip is resolved
	/// through [`find_input_footage`] and [`RenderTask::audio_media`]: the
	/// proxy file with stream 1 when proxy media is enabled and ready and
	/// carries audio, otherwise the original filename with stream 0. Audio
	/// gain/pan/fades come from [`MontageAudio::from_block_core`] (gain
	/// clamped to 0..4, pan to -1..1, negative/NaN fades normalised to no
	/// fade) and the clip's own audio chain is carried in `audio_effects`;
	/// the video `effects` list is always empty here. Track order is
	/// irrelevant because the mixer accumulates gains.
	///
	/// `project` is locked for the whole walk (a poisoned lock is recovered,
	/// not propagated). Returns an empty vector when `sequence` is not in the
	/// graph, is not a sequence, or no audio clip covers `time` — a missing
	/// or non-audio footage node on one clip simply skips that clip.
	// Only the unit tests use the point query; export uses `audio_montage_in`.
	#[cfg(test)]
	fn audio_montage(
		project: &ProjectRef,
		sequence: id::NodeId,
		time: Rational,
	) -> Vec<MontageClip> {
		// A point query is the half-open range [time, time + 1µs).
		Self::audio_montage_in(project, sequence, time, time + Rational::new(1, 1_000_000))
	}

	/// Resolve every audible clip of `sequence` that overlaps the half-open
	/// range `[range_in, range_out)`.
	///
	/// Export renders the audio of a whole range in one ticket, so the
	/// montage must contain every clip overlapping that range. Filtering on
	/// the range start alone dropped every clip that begins after it: in an
	/// export starting at 0 only the clips playing at 0 s were mixed, and
	/// audio placed later came out silent. The preview path
	/// (`preview_audio::audio_montage`) already filters on overlap.
	fn audio_montage_in(
		project: &ProjectRef,
		sequence: id::NodeId,
		range_in: Rational,
		range_out: Rational,
	) -> Vec<MontageClip> {
		let guard = project.lock().unwrap_or_else(|e| e.into_inner());
		let Some(entry) = guard.graph.get(sequence) else {
			return Vec::new();
		};
		let Some(seq) = entry
			.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<SequenceBehavior>())
		else {
			return Vec::new();
		};
		let mut montage = Vec::new();
		for list_id in seq.track_lists.iter().filter(|i| i.valid()) {
			let Some(le) = guard.graph.get(*list_id) else {
				continue;
			};
			let Some(list) = le
				.behavior
				.as_any()
				.and_then(|a| a.downcast_ref::<TrackListBehavior>())
			else {
				continue;
			};
			if list.kind != TrackType::Audio {
				continue;
			}
			for track_id in &list.tracks {
				let Some(te) = guard.graph.get(*track_id) else {
					continue;
				};
				let Some(track) = te
					.behavior
					.as_any()
					.and_then(|a| a.downcast_ref::<TrackBehavior>())
				else {
					continue;
				};
				if track.muted {
					continue;
				}
				for block_id in &track.blocks {
					let Some(core) = crate::nodeops::block_core_of(&guard.graph, *block_id) else {
						continue;
					};
					if core.out() <= range_in || core.in_() >= range_out {
						continue;
					}
					let Some(footage_id) = find_input_footage(&guard.graph, *block_id) else {
						continue;
					};
					let Some(fe) = guard.graph.get(footage_id) else {
						continue;
					};
					let Some(footage) = fe
						.behavior
						.as_any()
						.and_then(|a| a.downcast_ref::<FootageBehavior>())
					else {
						continue;
					};
					let Some((filename, stream_index)) = Self::audio_media(footage) else {
						continue;
					};
					let audio = MontageAudio::from_block_core(&core);
					montage.push(MontageClip {
						filename,
						stream_index,
						in_time: core.in_(),
						out_time: core.out(),
						media_in: core.media_in,
						gain: audio.gain,
						pan: audio.pan,
						fade_in: audio.fade_in,
						fade_out: audio.fade_out,
						// Audio clips carry no video effect stack; the
						// clip's audio chain is `audio_effects` below.
						effects: Vec::new(),
						audio_effects: audio.effects,
					});
				}
			}
		}
		montage
	}

	/// Build the video ticket params for the frame at `time` (mirrors the
	/// C++ `start_video_ticket` param marshalling).
	///
	/// The viewer node decides the shape of the ticket. A footage viewer
	/// yields a single-source ticket: `footage = (filename, 0)` and the frame
	/// cache keyed by the viewer node identity (see
	/// [`RenderTask::set_render_inputs`] on pre-cache runs). A sequence
	/// viewer leaves `footage` empty and fills `montage` from
	/// [`RenderTask::video_montage`], which is resolved *after* the project
	/// lock is released — the same lock is not reentrant, so holding it
	/// across the montage build would deadlock every sequence export. The
	/// owning project's uuid is copied out first and travels with the ticket
	/// so a worker renders only from its own project's snapshot.
	/// `force_size`/`force_format` come from the [`ForceParams`] of this
	/// task; `adjustments` is left empty.
	///
	/// `time` is in the sequence timebase and is passed through unchanged.
	///
	/// # Errors
	///
	/// Returns `Error::Failed("No node connected to the viewer output")` when
	/// the viewer id is stale, or when the node behind it is neither footage
	/// nor a sequence.
	fn build_video_ticket(&self, time: Rational) -> Result<VideoTicketParams> {
		let (project, viewer_id) = &self.viewer;
		// The owning project's uuid (M16 S1: graph-mode tickets carry it so
		// workers render only from their own project's snapshot). Read
		// without holding the lock across the montage build below
		// (`std::sync::Mutex` is not reentrant).
		let project_uuid = project
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.uuid
			.clone();
		// Inspect the viewer node WITHOUT holding the project lock across
		// the montage build below: `video_montage` locks the same project,
		// and `std::sync::Mutex` is not reentrant — holding it here
		// self-deadlocks every sequence export on the driving thread
		// (reproduced by the facade's `it_export` suite). The borrow
		// (`footage.filename`) is cloned before the lock drops.
		let footage = {
			let guard = project.lock().unwrap_or_else(|e| e.into_inner());
			let Some(entry) = guard.graph.get(*viewer_id) else {
				return Err(Error::Failed(
					"No node connected to the viewer output".to_string(),
				));
			};
			let any = entry.behavior.as_any();
			if let Some(footage) = any.and_then(|a| a.downcast_ref::<FootageBehavior>()) {
				Some(footage.filename.clone())
			} else if any
				.and_then(|a| a.downcast_ref::<SequenceBehavior>())
				.is_some()
			{
				None
			} else {
				return Err(Error::Failed(
					"No node connected to the viewer output".to_string(),
				));
			}
		};
		match footage {
			Some(filename) => Ok(VideoTicketParams {
				viewer: viewer_id.identity(),
				project: project_uuid.clone(),
				time,
				force_size: self.force_size(),
				force_format: self.force_format(),
				cache: Some(viewer_id.identity()),
				cache_dir: None,
				cache_id: None,
				cache_timebase: None,
				footage: Some((filename, 0)),
				montage: Vec::new(),
				adjustments: Vec::new(),
			}),
			// Sequence viewer: the montage is resolved without the lock.
			None => {
				let montage = Self::video_montage(project, *viewer_id, time);
				Ok(VideoTicketParams {
					viewer: viewer_id.identity(),
					project: project_uuid.clone(),
					time,
					force_size: self.force_size(),
					force_format: self.force_format(),
					cache: None,
					cache_dir: None,
					cache_id: None,
					cache_timebase: None,
					footage: None,
					montage,
					adjustments: Vec::new(),
				})
			}
		}
	}

	/// Build the audio ticket params for `range` (the Rust API renders a
	/// single range; the C++ submits one ticket per audio range).
	fn build_audio_ticket(&self, range: TimeRange) -> Result<AudioTicketParams> {
		let (project, viewer_id) = &self.viewer;
		let (sample_rate, channel_layout) =
			crate::nodeops::sequence_audio_params(project, *viewer_id);
		let montage = Self::audio_montage_in(project, *viewer_id, range.in_(), range.out());
		let bus_effects = {
			let guard = project.lock().unwrap_or_else(|e| e.into_inner());
			audio::load_bus_effects(&guard.settings, viewer_id.identity())
		};
		Ok(AudioTicketParams {
			viewer: viewer_id.identity(),
			range,
			sample_rate,
			channel_layout,
			montage,
			bus_effects,
		})
	}

	/// Submit one video frame ticket at `time` (mirrors the C++
	/// `start_video_ticket`). `dispatch` is the shared completion channel
	/// handed to the ticket's finished callback. M15 S2: export/precache
	/// tickets post at Background priority (the scheduler serves them when
	/// no Seek/Playback work is pending; credit flow control caps how many
	/// are in flight).
	fn submit_video_ticket(
		&self,
		arena: &TicketArena,
		time: Rational,
		dispatch: *mut RenderDispatch,
	) -> Result<TicketId> {
		let params = self.build_video_ticket(time)?;
		let id = arena.next_id();
		let dispatch_ptr = DispatchPtr(dispatch);
		arena.submit_video_background_with_id(
			id,
			params,
			Box::new(move |result| {
				push_finished(id, result, dispatch_ptr);
			}),
		);
		Ok(id)
	}

	/// Submit one audio ticket for `range` (see
	/// [`RenderTask::submit_video_ticket`]).
	fn submit_audio_ticket(
		&self,
		arena: &TicketArena,
		range: TimeRange,
		dispatch: *mut RenderDispatch,
	) -> Result<TicketId> {
		let params = self.build_audio_ticket(range)?;
		let id = arena.next_id();
		let dispatch_ptr = DispatchPtr(dispatch);
		arena.submit_audio_with_id(
			id,
			params,
			Box::new(move |result| {
				push_finished(id, result, dispatch_ptr);
			}),
		);
		Ok(id)
	}

	/// Submit one video frame ticket at `time` and account it as in flight.
	///
	/// `arena` is the shared ticket arena; `time` is the frame's timestamp in
	/// the sequence timebase; `dispatch` is the completion channel handed to
	/// the ticket and must outlive its callback (the loop owns it). On
	/// success the id is pushed to `in_flight` (so the failure path can
	/// cancel and wait it) and recorded in `ticket_keys` under
	/// [`TICKET_VIDEO`] with the time's numerator/denominator, which is the
	/// key [`RenderTask::classify_ticket`] uses to find the delivery slot.
	///
	/// The in-flight count is bumped **before** the submit: a ticket can
	/// complete synchronously (its callback fires before the submit returns),
	/// so the callback's decrement must always see its increment. A failed
	/// submit never fires a callback, so the bump is rolled back. Errors come
	/// from [`RenderTask::build_video_ticket`] (e.g. nothing connected to the
	/// viewer) and leave both the counter and the bookkeeping untouched.
	fn start_video_ticket(
		&self,
		arena: &TicketArena,
		time: Rational,
		dispatch: *mut RenderDispatch,
		in_flight: &mut Vec<TicketId>,
		ticket_keys: &mut HashMap<TicketId, (i32, i64, i64)>,
	) -> Result<()> {
		unsafe {
			(&*dispatch).running.fetch_add(1, Ordering::SeqCst);
		}
		match self.submit_video_ticket(arena, time, dispatch) {
			Ok(id) => {
				in_flight.push(id);
				ticket_keys.insert(id, (TICKET_VIDEO, time.numerator(), time.denominator()));
				Ok(())
			}
			Err(e) => {
				unsafe {
					(&*dispatch).running.fetch_sub(1, Ordering::SeqCst);
				}
				Err(e)
			}
		}
	}

	/// Submit the audio ticket for `range` and account it as in flight.
	///
	/// Same counting contract as [`RenderTask::start_video_ticket`]: the
	/// `dispatch.running` counter is incremented **before** the submit
	/// (a completion callback may fire synchronously), rolled back when the
	/// submit itself fails, and decremented later by the ticket's finished
	/// callback, so the render loop sees one net increment per live ticket.
	///
	/// `range` is the audio range to pull; `in_flight` receives the new id so
	/// the loop can cancel/wait it on error; `dispatch` is the shared
	/// completion channel and must outlive the callback. `ticket_keys`
	/// records the id under [`TICKET_AUDIO`] with the range's in-point
	/// numerator/denominator — the key [`RenderTask::classify_ticket`] later
	/// uses to route the result to the audio delivery slot. Errors come from
	/// [`RenderTask::build_audio_ticket`].
	fn start_audio_ticket(
		&self,
		arena: &TicketArena,
		range: TimeRange,
		dispatch: *mut RenderDispatch,
		in_flight: &mut Vec<TicketId>,
		ticket_keys: &mut HashMap<TicketId, (i32, i64, i64)>,
	) -> Result<()> {
		unsafe {
			(&*dispatch).running.fetch_add(1, Ordering::SeqCst);
		}
		match self.submit_audio_ticket(arena, range, dispatch) {
			Ok(id) => {
				in_flight.push(id);
				ticket_keys.insert(
					id,
					(
						TICKET_AUDIO,
						range.in_().numerator(),
						range.in_().denominator(),
					),
				);
				Ok(())
			}
			Err(e) => {
				unsafe {
					(&*dispatch).running.fetch_sub(1, Ordering::SeqCst);
				}
				Err(e)
			}
		}
	}

	/// Map a finished ticket back to its delivery slot: the audio slot for
	/// audio tickets, the matching frame slot for video tickets. The key
	/// comes from `ticket_keys` (the submitter-side record) — NOT from the
	/// arena: the arena reaps finished fire-and-forget slots on the next
	/// `allocate()`, so a ticket that completed before the next submit is
	/// already gone from the arena's own map (the "Render ticket reported
	/// an unexpected timestamp" failure that killed every export).
	fn classify_ticket(
		&self,
		ticket_keys: &HashMap<TicketId, (i32, i64, i64)>,
		id: TicketId,
		slot_by_key: &HashMap<(i32, i64, i64), usize>,
	) -> Option<usize> {
		let key = ticket_keys.get(&id)?;
		slot_by_key.get(key).copied()
	}

	/// Drive the whole render: keep up to `max_inflight` frame tickets in
	/// flight (audio first, then frames in timestamp order), deliver each
	/// finished ticket's result to the behavior hooks in timestamp order via
	/// the reorder buffer, and stop on cancellation or a hook error —
	/// cancelling and waiting every still-running ticket so their
	/// completions fire exactly once.
	///
	/// `task` is the live driving task (cancellation/progress/error
	/// reporting); `behavior` is the concrete subclass receiving the
	/// `frame_downloaded`/`audio_downloaded` hooks.
	pub fn render(&mut self, task: &mut Task, behavior: &mut dyn RenderTaskBehavior) -> Result<()> {
		let timebase = self.timebase();

		// Compute the frame timestamps (progress denominator mirrors the
		// C++ `total_length <= 0 -> 1` guard).
		let mut frame_times: Vec<Rational> = Vec::new();
		if self.video_enabled {
			let mut t = self.export_range.in_();
			while t < self.export_range.out() {
				frame_times.push(t);
				t = t + timebase;
			}
		}
		let video_end = self.video_end.unwrap_or(self.export_range.out());
		let live_frames = frame_times.iter().take_while(|t| **t < video_end).count();
		self.total_frames = frame_times.len() as i64;
		let total_length = if frame_times.is_empty() {
			1.0
		} else {
			frame_times.len() as f64
		};

		// Delivery order: the audio range first (mirrors the C++ queue
		// order), then every frame in ascending timestamp order. The reorder
		// buffer delivers to the hooks in exactly this order no matter the
		// completion order.
		let mut slots: Vec<DeliverySlot> = Vec::new();
		if self.audio_enabled {
			slots.push(DeliverySlot {
				kind: TICKET_AUDIO,
				time: self.export_range.in_(),
			});
		}
		for (index, &ft) in frame_times.iter().enumerate() {
			slots.push(DeliverySlot {
				kind: if index < live_frames {
					TICKET_VIDEO
				} else {
					TICKET_BLACK
				},
				time: ft,
			});
		}
		let mut slot_by_key: HashMap<(i32, i64, i64), usize> = HashMap::with_capacity(slots.len());
		for (i, slot) in slots.iter().enumerate() {
			slot_by_key.insert(
				(slot.kind, slot.time.numerator(), slot.time.denominator()),
				i,
			);
		}
		let total_slots = slots.len();

		// The ticket arena: the process-wide manager arena when the
		// manager is initialized, otherwise a private process dispatcher +
		// arena owned by this run (keeps headless/test runs
		// self-contained; M15 S2 — the in-process thread pool is gone).
		// Zero-copy in-process rendering: the ticket producer runs on the
		// caller's render thread and returns the frame directly; there is
		// no worker process or shared-memory staging on this path.
		let arena = match manager::RenderManager::global() {
			Some(manager) => manager.tickets.clone(),
			None => {
				let dispatcher: Arc<dyn JobDispatch> = InlineDispatcher::sync();
				let producer: quercus_render::ticket::Producer = Arc::new(|time, params| {
					eval::render_produced_frame(time, params).map(TicketPayload::Video)
				});
				Arc::new(TicketArena::new_with_audio_fallback(
					dispatcher.clone(),
					dispatcher.clone(),
					Some(dispatcher),
					producer,
				))
			}
		};
		let pump = || {
			if let Some(m) = manager::RenderManager::global() {
				m.poll();
			}
		};

		// The shared completion channel. Heap-allocated: the ticket
		// completion callback reaches it through its captured pointer.
		// Freed only once every in-flight ticket has fired its callback
		// (`running == 0`).
		let dispatch = Box::into_raw(Box::new(RenderDispatch::new()));
		// Shared view of the same state for the render thread (the raw
		// pointer stays valid until the box is freed below).
		let dispatch_ref = unsafe { &*dispatch };
		let window = self.max_inflight.max(1);

		// Tickets we hold (the submitter's ids); `consumed` counts tickets
		// whose finished queue copy has been popped, so
		// `in_flight.len() - consumed` is the live window.
		let mut in_flight: Vec<TicketId> = Vec::new();
		let mut consumed = 0usize;
		// Next frame timestamp to submit and next delivery slot.
		let mut next_frame_index = 0usize;
		let mut next_slot = 0usize;
		// Reorder buffer: finished tickets not yet deliverable.
		let mut pending: HashMap<usize, (TicketId, TicketResult)> = HashMap::new();
		// Submitter-side ticket id -> delivery-slot key (the arena reaps
		// finished fire-and-forget slots; this record is authoritative).
		let mut ticket_keys: HashMap<TicketId, (i32, i64, i64)> = HashMap::new();
		let mut progress_counter = 0.0;
		let mut result: Result<()> = Ok(());

		// Queue audio first (mirrors the C++ order).
		if self.audio_enabled && result.is_ok() {
			if let Err(e) = self.start_audio_ticket(
				&arena,
				self.export_range,
				dispatch,
				&mut in_flight,
				&mut ticket_keys,
			) {
				result = Err(e);
			}
		}

		// Start the initial frame window.
		if result.is_ok() {
			while in_flight.len() < window && next_frame_index < live_frames {
				if let Err(e) = self.start_video_ticket(
					&arena,
					frame_times[next_frame_index],
					dispatch,
					&mut in_flight,
					&mut ticket_keys,
				) {
					result = Err(e);
					break;
				}
				next_frame_index += 1;
			}
		}

		while result.is_ok() && !task.is_cancelled() && next_slot < total_slots {
			// Drain the completion queue into the reorder buffer.
			while let Some((id, ticket_result)) = dispatch_ref.pop_finished() {
				consumed += 1;
				match self.classify_ticket(&ticket_keys, id, &slot_by_key) {
					Some(slot_index) => {
						pending.insert(slot_index, (id, ticket_result));
					}
					None => {
						result = Err(Error::Failed(
							"Render ticket reported an unexpected timestamp".to_string(),
						));
						break;
					}
				}
			}
			if result.is_err() {
				break;
			}

			// Deliver contiguous results in the observable (timestamp) order.
			while next_slot < total_slots {
				if slots[next_slot].kind == TICKET_BLACK {
					match self.black_texture(slots[next_slot].time) {
						Some(texture) => {
							if let Err(e) = behavior.frame_downloaded(task, &texture) {
								result = Err(e);
								break;
							}
						}
						None => {
							result = Err(Error::Failed(
								"Could not allocate a black pad frame".to_string(),
							));
							break;
						}
					}
					if self.native_progress_signalling {
						progress_counter += 1.0;
						task.emit_progress(progress_counter / total_length);
					}
					next_slot += 1;
					continue;
				}
				let Some((_id, ticket_result)) = pending.remove(&next_slot) else {
					break;
				};
				let slot = &slots[next_slot];
				if slot.kind == TICKET_AUDIO {
					match ticket_result {
						Ok(TicketPayload::Audio(samples)) => {
							if let Err(e) = behavior.audio_downloaded(task, &samples) {
								result = Err(e);
								break;
							}
						}
						Ok(TicketPayload::ShmAudio(audio)) => {
							// M15 S3 process backend: the audio lives in a
							// worker shm slot. Copy the samples out (the
							// encoder needs an owned f32 buffer), hand them
							// to the behavior, then release the slot.
							let samples = audio.to_audio_samples();
							if let Err(e) = behavior.audio_downloaded(task, &samples) {
								result = Err(e);
								break;
							}
							if let Some(m) = manager::RenderManager::global() {
								m.release_audio_frame(&audio);
							}
						}
						Ok(_) => {
							result = Err(Error::Failed(
								"Audio render ticket delivered a non-audio payload".to_string(),
							));
							break;
						}
						Err(e) => {
							result =
								Err(Error::Failed(format!("Audio render ticket failed: {e:?}")));
							break;
						}
					}
				} else {
					match ticket_result {
						Ok(TicketPayload::Video(texture)) => {
							if let Err(e) = behavior.frame_downloaded(task, &texture) {
								result = Err(e);
								break;
							}
							if self.native_progress_signalling {
								progress_counter += 1.0;
								task.emit_progress(progress_counter / total_length);
							}
						}
						Ok(TicketPayload::ShmFrame(frame)) => {
							// M15 S2 process backend: the frame lives in a
							// worker shm slot. Copy it out once into a
							// texture the encoder consumes (necessary copy —
							// the encoder needs an owned F32 buffer), then
							// release the slot.
							let texture = shm_frame_to_texture(&frame);
							if let Err(e) = behavior.frame_downloaded(task, &texture) {
								result = Err(e);
								break;
							}
							if let Some(m) = manager::RenderManager::global() {
								m.release_frame(&frame);
							}
							if self.native_progress_signalling {
								progress_counter += 1.0;
								task.emit_progress(progress_counter / total_length);
							}
						}
						Ok(_) => {
							result = Err(Error::Failed(
								"Video render ticket delivered a non-video payload".to_string(),
							));
							break;
						}
						Err(e) => {
							result =
								Err(Error::Failed(format!("Frame render ticket failed: {e:?}")));
							break;
						}
					}
				}
				next_slot += 1;
			}
			if result.is_err() || task.is_cancelled() {
				break;
			}

			// Refill the window: one new ticket per delivered frame.
			while in_flight.len().saturating_sub(consumed) < window
				&& next_frame_index < live_frames
			{
				if let Err(e) = self.start_video_ticket(
					&arena,
					frame_times[next_frame_index],
					dispatch,
					&mut in_flight,
					&mut ticket_keys,
				) {
					result = Err(e);
					break;
				}
				next_frame_index += 1;
			}
			if result.is_err() {
				break;
			}
			if next_slot >= total_slots {
				break;
			}

			// Wait for the next completion (a cancellation or a hook error
			// aborts the wait; in-flight tickets then finish, waking us).
			// The process dispatcher is pumped so its poll loop delivers the
			// completions — never while holding the finished-queue lock
			// (pump()'s delivered completions lock the same queue).
			loop {
				pump();
				let mut guard = dispatch_ref
					.finished
					.lock()
					.unwrap_or_else(|e| e.into_inner());
				let done = !guard.is_empty()
					|| dispatch_ref.running.load(Ordering::SeqCst) == 0
					|| task.is_cancelled();
				if done {
					break;
				}
				let (g, _) = dispatch_ref
					.cv
					.wait_timeout(guard, time::Duration::from_millis(5))
					.unwrap_or_else(|e| e.into_inner());
				guard = g;
				drop(guard); // release before the next pump
			}
			if dispatch_ref
				.finished
				.lock()
				.unwrap_or_else(|e| e.into_inner())
				.is_empty()
				&& dispatch_ref.running.load(Ordering::SeqCst) == 0
			{
				// Every ticket finished and its queue copy was consumed.
				break;
			}
		}

		// Cancellation that aborted the loop before every slot was delivered
		// maps to the cancellation error (the sync loop returned
		// `Error::Cancelled` from its between-frames check; a render that
		// finished every frame despite a late cancel stays successful).
		if result.is_ok() && task.is_cancelled() && next_slot < total_slots {
			result = Err(Error::Cancelled);
		}

		// Tear down. On cancellation or error, cancel and wait every ticket
		// still in flight (the C++ abort path), so their completions still
		// fire exactly once. Then wait until every callback has returned
		// and free the completion channel; a private dispatcher is shut
		// down (M15 S2: the process dispatcher, not a thread pool).
		if result.is_err() || task.is_cancelled() {
			for &id in &in_flight {
				arena.cancel(id);
				let _ = arena.wait(id);
			}
		}
		dispatch_ref.wait_idle(&pump);
		unsafe {
			drop(Box::from_raw(dispatch));
		}
		result
	}

	/// A detached render state used while a concrete task temporarily moves
	/// its render out of itself to drive it with itself as the behavior
	/// (avoids a self-referential borrow). Never used for actual rendering.
	pub(crate) fn placeholder() -> RenderTask {
		RenderTask::new(
			Task::new("", None),
			None,
			(project::Project::new(), id::NodeId::INVALID),
			ForceParams::default(),
			None,
		)
	}
}

/// One deliverable unit of the render: an audio range or a frame time, in
/// the observable delivery order (audio first, then frames in time order).
struct DeliverySlot {
	/// `TICKET_AUDIO` or `TICKET_VIDEO`.
	kind: i32,
	/// Frame time (video) or range start (audio).
	time: Rational,
}

/// Shared state between the render thread and the ticket-finished
/// callbacks (the ticket's async return channel, mirroring the C++
/// `finished_mutex_`/`finished_tickets_`/`finished_wait_cond_`). One
/// instance per [`RenderTask::render`] run, reached from the completion
/// callback through its captured pointer; heap-allocated for the run and
/// freed only after every in-flight ticket has fired (`running == 0`).
struct RenderDispatch {
	/// Finished tickets in completion order (callbacks push here).
	finished: Mutex<VecDeque<(TicketId, TicketResult)>>,
	/// Tickets whose completion callback has not fired yet.
	running: AtomicUsize,
	/// Wakes the render thread when a ticket finishes.
	cv: Condvar,
}

impl RenderDispatch {
	fn new() -> RenderDispatch {
		RenderDispatch {
			finished: Mutex::new(VecDeque::new()),
			running: AtomicUsize::new(0),
			cv: Condvar::new(),
		}
	}

	/// Pop the next finished ticket; `None` when the queue is empty.
	fn pop_finished(&self) -> Option<(TicketId, TicketResult)> {
		self.finished
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.pop_front()
	}

	/// Block until every submitted ticket has fired its callback. Called
	/// before freeing `self`, so no callback can touch the state afterwards.
	/// `pump` drives the process dispatcher's poll loop (M15 S2) — never
	/// while holding the finished-queue lock (pump's delivered completions
	/// lock the same queue).
	fn wait_idle(&self, pump: &dyn Fn()) {
		loop {
			pump();
			let mut guard = self.finished.lock().unwrap_or_else(|e| e.into_inner());
			if self.running.load(Ordering::SeqCst) == 0 {
				break;
			}
			let (g, _) = self
				.cv
				.wait_timeout(guard, time::Duration::from_millis(5))
				.unwrap_or_else(|e| e.into_inner());
			guard = g;
			drop(guard); // release before the next pump
		}
	}
}

/// `Send` wrapper for the raw dispatch pointer captured by the ticket
/// completion callbacks (the arena's [`quercus_render::ticket::Completion`] is
/// `Send`; the pointee outlives every ticket — see [`RenderTask::render`]'s
/// `wait_idle` before freeing it).
struct DispatchPtr(*mut RenderDispatch);

// Safety: the raw pointer is exclusively owned by the render run; the
// completion callbacks only dereference it while the run is alive
// (`wait_idle` guarantees every callback returned before the free).
unsafe impl Send for DispatchPtr {}

/// Ticket-finished callback, mirroring the C++ `on_ticket_finished`. Fires
/// on the ticket's finishing thread; pushes the ticket's result into the
/// completion queue and wakes the render thread. The push precedes the
/// in-flight decrement, so a waiter never observes `running == 0` with a
/// missing queue entry.
///
/// # Safety
///
/// The wrapped pointer must point to a live `RenderDispatch` for the
/// duration of every in-flight ticket (guaranteed by
/// [`RenderTask::render`]'s `wait_idle` before freeing it).
fn push_finished(id: TicketId, result: TicketResult, dispatch: DispatchPtr) {
	let dispatch = unsafe { &*dispatch.0 };
	let mut queue = dispatch.finished.lock().unwrap_or_else(|e| e.into_inner());
	queue.push_back((id, result));
	dispatch.running.fetch_sub(1, Ordering::SeqCst);
	dispatch.cv.notify_all();
}

/// Copy a process-backend shm frame out into an F32 CPU texture the
/// encoder consumes (M15 S2/S3). F32 slots (a forced F32 export ticket)
/// are read straight out — their bytes are already f32 RGBA little-endian,
/// no conversion; BGRA8 slots (the default preview path) convert once with
/// [`bgra8_to_f32_rgba`].
fn shm_frame_to_texture(frame: &ShmFrameRef) -> quercus_core::texture::Texture {
	let meta = &frame.meta;
	let pixels = frame
		.shm
		.slot_bytes(frame.slot)
		.get(..meta.data_size.max(0) as usize)
		.unwrap_or_default();
	if meta.format == PixelFormat::F32 as i32 {
		// F32 slot: the encoder gets the pipeline samples with no round trip.
		let mut f = quercus_core::texture::Frame::new();
		f.width = meta.width;
		f.height = meta.height;
		f.format = PixelFormat::F32;
		f.channels = 4;
		f.timestamp = quercus_core::Rational::new(meta.time_num, meta.time_den);
		f.data = pixels.to_vec();
		return quercus_core::texture::Texture::wrap_frame(f);
	}
	let samples = bgra8_to_f32_rgba(pixels);
	let mut f = quercus_core::texture::Frame::new();
	f.width = meta.width;
	f.height = meta.height;
	f.format = PixelFormat::F32;
	f.channels = 4;
	f.timestamp = quercus_core::Rational::new(meta.time_num, meta.time_den);
	f.data = samples
		.as_chunks::<4>()
		.0
		.iter()
		.flat_map(|px| {
			let mut bytes = [0u8; 16];
			for (i, v) in px.iter().enumerate() {
				bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
			}
			bytes
		})
		.collect();
	quercus_core::texture::Texture::wrap_frame(f)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod audio_export_tests {
	use super::*;
	use crate::nodeops::{node_length, video_length, ProjectRef};
	use quercus_node::footage::StreamInfo;
	use quercus_node::id::NodeId;
	use quercus_node::track::TrackType;

	fn project() -> ProjectRef {
		quercus_node::project::Project::new()
	}

	fn footage_with_streams(project: &ProjectRef, name: &str) -> NodeId {
		let (core, behavior) = quercus_node::footage::FootageBehavior::create();
		let id = project.lock().unwrap().graph.add_node(core, behavior);
		let mut guard = project.lock().unwrap();
		let f = guard
			.graph
			.get_mut(id)
			.and_then(|e| e.behavior.as_any_mut())
			.and_then(|a| a.downcast_mut::<quercus_node::footage::FootageBehavior>())
			.unwrap();
		f.filename = name.to_string();
		f.streams.push(StreamInfo {
			index: 0,
			is_video: true,
			video: None,
			audio: None,
			duration: Rational::new(5, 1),
		});
		f.streams.push(StreamInfo {
			index: 2,
			is_video: false,
			video: None,
			audio: None,
			duration: Rational::new(5, 1),
		});
		id
	}

	fn sequence_with_track(project: &ProjectRef, kind: TrackType) -> (NodeId, NodeId) {
		let (core, behavior) = quercus_node::sequence::SequenceBehavior::create();
		let sequence = project.lock().unwrap().graph.add_node(core, behavior);
		let (core, behavior) = quercus_node::track::TrackListBehavior::create();
		let list = project.lock().unwrap().graph.add_node(core, behavior);
		let (core, behavior) = quercus_node::track::TrackBehavior::create();
		let track = project.lock().unwrap().graph.add_node(core, behavior);
		let mut guard = project.lock().unwrap();
		guard
			.graph
			.get_mut(list)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TrackListBehavior>()
			.unwrap()
			.kind = kind;
		guard
			.graph
			.get_mut(track)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TrackBehavior>()
			.unwrap()
			.kind = kind;
		guard
			.graph
			.get_mut(sequence)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<SequenceBehavior>()
			.unwrap()
			.track_lists
			.push(list);
		guard
			.graph
			.get_mut(list)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TrackListBehavior>()
			.unwrap()
			.tracks
			.push(track);
		(sequence, track)
	}

	fn clip_on(project: &ProjectRef, footage: NodeId, track: NodeId, range: TimeRange) -> NodeId {
		let (core, behavior) = quercus_node::block::clip_create();
		let clip = project.lock().unwrap().graph.add_node(core, behavior);
		{
			let mut guard = project.lock().unwrap();
			guard
				.graph
				.get_mut(clip)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<quercus_node::block::ClipBlockBehavior>()
				.unwrap()
				.core
				.range = range;
		}
		crate::nodeops::clip_set_footage(project, clip, footage);
		crate::nodeops::track_append_block(project, track, clip);
		clip
	}

	#[test]
	fn audio_montage_uses_the_audio_stream_and_skips_muted() {
		let project = project();
		let (sequence, track) = sequence_with_track(&project, TrackType::Audio);
		let footage = footage_with_streams(&project, "clip.mp4");
		clip_on(
			&project,
			footage,
			track,
			TimeRange::new(Rational::new(0, 1), Rational::new(2, 1)),
		);
		let montage = RenderTask::audio_montage(&project, sequence, Rational::new(1, 1));
		assert_eq!(montage.len(), 1);
		assert_eq!(montage[0].filename, "clip.mp4");
		assert_eq!(montage[0].stream_index, 2, "stream 0 is video");

		{
			let mut guard = project.lock().unwrap();
			guard
				.graph
				.get_mut(track)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackBehavior>()
				.unwrap()
				.muted = true;
		}
		assert!(RenderTask::audio_montage(&project, sequence, Rational::new(1, 1)).is_empty());
	}

	#[test]
	fn video_montage_skips_muted_tracks_and_video_length_ignores_audio() {
		let project = project();
		let (sequence, track) = sequence_with_track(&project, TrackType::Video);
		let footage = footage_with_streams(&project, "clip.mp4");
		clip_on(
			&project,
			footage,
			track,
			TimeRange::new(Rational::new(0, 1), Rational::new(2, 1)),
		);
		assert_eq!(
			RenderTask::video_montage(&project, sequence, Rational::new(1, 1)).len(),
			1
		);
		{
			let mut guard = project.lock().unwrap();
			guard
				.graph
				.get_mut(track)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackBehavior>()
				.unwrap()
				.muted = true;
		}
		assert!(RenderTask::video_montage(&project, sequence, Rational::new(1, 1)).is_empty());

		assert_eq!(node_length(&project, sequence), Rational::new(2, 1));
		assert_eq!(video_length(&project, sequence), Rational::new(2, 1));
	}
}

#[cfg(test)]
mod keyframe_export_tests {
	use super::*;
	use quercus_node::keyframe::{Interpolation, Keyframe};
	use quercus_node::node::ENABLED_INPUT;
	use quercus_node::value::NodeValue;

	#[test]
	fn clip_effects_samples_keyframes_at_the_requested_time() {
		let project = quercus_node::project::Project::new();
		let footage = {
			let (core, behavior) = quercus_node::footage::FootageBehavior::create();
			project.lock().unwrap().graph.add_node(core, behavior)
		};
		let (ccore, cbehavior) = quercus_node::block::clip_create();
		let clip = project.lock().unwrap().graph.add_node(ccore, cbehavior);
		let (ecore, ebehavior) = quercus_node::factory::Factory::global()
			.create_any("org.olivevideoeditor.Olive.opacity")
			.expect("opacity registered");
		let mut guard = project.lock().unwrap();
		let effect = guard.graph.add_node(ecore, ebehavior);
		guard
			.graph
			.get_mut(effect)
			.unwrap()
			.core
			.set_standard_value(ENABLED_INPUT, -1, NodeValue::Boolean(true));
		// Static standard value 0.5, overridden by keyframes 0 -> 1.
		guard
			.graph
			.get_mut(effect)
			.unwrap()
			.core
			.set_standard_value("opacity_in", -1, NodeValue::Float(0.5));
		guard
			.graph
			.get_mut(effect)
			.unwrap()
			.core
			.keyframe_track_mut("opacity_in", -1)
			.set_key(Keyframe {
				time: Rational::new(0, 1),
				value: NodeValue::Float(0.0),
				interpolation: Interpolation::Linear,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		guard
			.graph
			.get_mut(effect)
			.unwrap()
			.core
			.keyframe_track_mut("opacity_in", -1)
			.set_key(Keyframe {
				time: Rational::new(2, 1),
				value: NodeValue::Float(1.0),
				interpolation: Interpolation::Linear,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		guard.graph.connect(footage, effect, "tex_in", -1).unwrap();
		guard
			.graph
			.connect(
				effect,
				clip,
				quercus_node::block::clip_input::TEXTURE_INPUT,
				-1,
			)
			.unwrap();

		let value_at = |time: Rational| {
			let effects = RenderTask::clip_effects(&guard.graph, clip, time);
			let effect = effects.first().expect("effect");
			effect
				.params
				.iter()
				.find(|(id, _)| id == "opacity_in")
				.map(|(_, value)| value.to_double())
				.expect("opacity param")
		};
		assert!((value_at(Rational::new(0, 1)) - 0.0).abs() < 1e-9);
		assert!((value_at(Rational::new(1, 1)) - 0.5).abs() < 1e-9);
		assert!((value_at(Rational::new(2, 1)) - 1.0).abs() < 1e-9);
	}
}
