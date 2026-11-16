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

//! The evaluation seam (C++ `RenderProcessor : NodeTraverser`,
//! flattened): turns node-graph evaluation into render jobs by
//! implementing quercusnode's `RenderHooks`. Each C++ `process_*` virtual
//! is one hook method.
//!
//! This pass implements the graph hooks: frame generation runs fully;
//! plugin jobs dispatch through the executor slot quercusplugin installs
//! ([`set_plugin_executor`]); footage jobs decode through the quercuscodec
//! decoder bridge; shader jobs execute on the shared GPU context
//! ([`quercus_core::backend::GpuContext`], falling back to an input pass-
//! through when no adapter is available); color transform jobs apply
//! their OCIO processor (CPU frames convert for real; the GPU
//! color-managed blit is deferred at the backend and passes through);
//! cache jobs read their frame from the self-describing container in
//! [`crate::frameio`] (libquercusoiio EXR/JPEG pending) and otherwise
//! substitute the value the cache node was fed. Resolution is one pass
//! over the output table ([`RenderHooks::resolve`]) that runs each
//! boxed [`quercus_node::jobs::Job`] — and, first, the jobs nested in its
//! inputs — then replaces the box with the resulting texture.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::shaderfx::{compile_effect, run_effect};
use quercus_codec::decoder::{
	CodecStream, RenderMode, RetrieveAudioStatus, RetrieveVideoParams, K_COLOR_RANGE_DEFAULT,
};
use quercus_codec::ffmpeg::FFmpegDecoder;
use quercus_core::frame::VideoParamsPod;
use quercus_core::texture::{Frame, Texture};
use quercus_core::{PixelFormat, Rational, TimeRange};
use quercus_node::jobs::{
	CacheJobPayload, ColorTransformJobPayload, FootageJobPayload, Job, ShaderJobPayload,
};
use quercus_node::nodes::plugin::PluginJobPayload;
use quercus_node::value::{NodeValue, NodeValueRow, NodeValueTable};

/// Static mapping of OCIO-based node shaders to the OCIO function they
/// splice at their `%1` marker: (node type id, OCIO function name, source
/// space/role, destination space/role) — the C++ `GenerateProcessor`
/// `ColorTransform` pairs, resolved by the OCIO config at runtime.
///
/// Only the node's *function* (the GPU stub) is requested here; the
/// processor is built against the manager's reference color space
/// (C++ chromakey.cpp `GenerateProcessor` uses `GetReferenceColorSpace`,
/// which `ColorManager` sets to `OCIO::ROLE_SCENE_LINEAR`,
/// colormanager.cpp).
pub const OCIO_SHADER_STUBS: &[(&str, &str, &str, &str)] = &[(
	"org.olivevideoeditor.Olive.chromakey",
	"SceneLinearToCIEXYZ_d65",
	"scene_linear",
	"cie_xyz_d65_interchange",
)];

/// Static mapping of the OCIO grading nodes to the grading-primary style
/// whose dynamic GPU shader they splice at their `%1` marker (C++
/// `quercusrender_color_processor_create_grading_primary` with the node's
/// `GRADING_LIN`/`GRADING_LOG` style; the processor is built against the
/// default config at render time).
pub const OCIO_GRADING_STUBS: &[(&str, quercus_core::color::GradingStyle)] = &[
	(
		"org.olivevideoeditor.Olive.ociogradingtransformlinear",
		quercus_core::color::GradingStyle::Lin,
	),
	(
		"org.olivevideoeditor.Olive.OCIO_NAMESPACEgradingtransformlog",
		quercus_core::color::GradingStyle::Log,
	),
];

/// The OCIO GPU function shader for `type_id` (the `%1` stub), or `None`
/// when the node is not OCIO-based or the processor cannot be generated
/// (no default config, or a LUT processor the renderer cannot upload).
mod audio;
mod job_hooks;

#[cfg(test)]
pub(crate) use audio::{
	apply_audio_effect, apply_bus_effects, mix_audio_montage, process_clip_audio,
};
pub use audio::{audio_samples_byte_len, render_audio_samples, render_audio_samples_into};

pub fn ocio_stub_for(type_id: &str) -> Option<String> {
	let (_, fn_name, from, to) = OCIO_SHADER_STUBS
		.iter()
		.find(|(id, ..)| *id == type_id)
		.copied()?;
	quercus_core::color::ocio_function_shader(fn_name, from, to)
}

/// The OCIO grading GPU shader for `type_id` (the `%1` stub), or `None`
/// when the node is not a grading node or no default config is set up.
pub fn grading_stub_for(type_id: &str) -> Option<String> {
	let (_, style) = OCIO_GRADING_STUBS
		.iter()
		.find(|(id, _)| *id == type_id)
		.copied()?;
	quercus_core::color::grading_primary_function_shader(style)
}

/// Job specification: the closed set of C++ `*Job` payloads
/// (AcceleratedJob family) as internal evaluation records — jobs no
/// longer travel inside values across module boundaries.
#[derive(Clone, Debug)]
pub enum JobSpec {
	/// Shader job (frag/vert source + params).
	Shader {
		/// Fragment source.
		frag: String,
		/// Vertex source.
		vert: String,
	},
	/// Color transform job.
	ColorTransform {
		/// Processor identity (color::ProcessorCache key).
		processor: u64,
	},
	/// Direct frame generation (CPU nodes).
	Generate,
	/// Footage decode (C++ FootageJob; decode via bridge::codec).
	Footage {
		/// Decoder/stream id.
		decoder_id: String,
		/// Footage filename (M12 P0: the decode path).
		filename: String,
		/// Media stream index.
		stream_index: i32,
	},
	/// Sample generation (C++ SampleJob).
	Sample,
	/// OFX plugin job — executed through the registered plugin executor
	/// ([`set_plugin_executor`]; the quercusplugin crate installs its
	/// render driver there, so quercusrender never sees OFX types).
	Plugin {
		/// Plugin instance identity (quercusplugin instance registry key).
		instance: u64,
		/// OFX plugin identifier (cross-process stable). The single OFX
		/// host process (M3) resolves its own instance from this; the
		/// in-process executor ignores it and uses `instance`.
		type_id: String,
		/// Request time in seconds (C++ `PluginJob` time).
		time: f64,
		/// Clip name the main source texture arrives on (C++
		/// `node->get_effect_input_id()`).
		effect_input_id: Option<String>,
		/// Clip input textures by clip name (multi-input plugins).
		inputs: Vec<(String, Texture)>,
		/// Param overrides: input id -> node value (the tagged values
		/// captured at evaluation time).
		values: Vec<(String, NodeValue)>,
	},
}

/// The hooks implementation handed to the quercusnode traverser.
pub struct RenderEvalHooks {
	/// Cache usage toggle (C++ use_cache).
	pub use_cache: bool,
	/// Active ticket identity (for cancellation polling).
	pub ticket: Option<crate::ticket::TicketId>,
	/// Forced output size for resolved footage jobs; `None` decodes at
	/// the media's native size (C++ `RenderProcessor` requests the
	/// texture at the output resolution). The graph-sequence driver sets
	/// this to the sequence frame size.
	pub frame_size: Option<(i32, i32)>,
	/// The sequence's square-pixel resolution (C++ the `NodeGlobals`
	/// square resolution the shape/polygon generators insert as
	/// `resolution_in` at job-build time). Generator jobs anchor their
	/// pixel-size params to this — NOT to the render-target size — so a
	/// proxy-resolution playback render draws them at the same relative
	/// size as a paused full-res frame. `None` (non-sequence renders)
	/// falls back to the render-target size.
	pub sequence_size: Option<(i32, i32)>,
	/// Position of the adjustment layer currently being swept, in
	/// `0.0..=1.0` (C++ the adjustment/transition `progress_in` uniform).
	/// [`render_graph_frame`] sets this for the duration of one adjustment
	/// block's evaluation, so a chain hanging off an adjustment layer can
	/// fade its result across the layer's span. Only shaders declaring a
	/// `progress_in` uniform consume it.
	pub layer_progress: Option<f64>,
}

// ---------------------------------------------------------------------------
// Plugin job executor (dependency inversion seam)
// ---------------------------------------------------------------------------
//
// quercusrender sits BELOW quercusplugin in the dependency graph (quercusplugin
// depends on quercusrender for the texture value types), so the plugin job
// execution cannot be a direct call. The quercusplugin crate installs its
// render driver here at init; `process_plugin_job` dispatches through
// the slot. Without an executor, plugin jobs fail explainably (the
// pre-wiring behavior).

/// Plugin job request handed to the registered executor (the C++
/// `process_plugin_job(texture, destination, node)` inputs flattened).
pub struct PluginJobRequest<'a> {
	/// The job spec ([`JobSpec::Plugin`] guaranteed by the caller).
	pub spec: &'a JobSpec,
	/// The input texture the job runs against.
	pub src: Texture,
}

/// Plugin executor: runs one plugin job and returns the output
/// texture. Implemented by the quercusplugin crate on top of its render
/// driver.
pub type PluginExecutor = dyn Fn(&PluginJobRequest<'_>) -> Result<Texture> + Send + Sync;

static PLUGIN_EXECUTOR: std::sync::OnceLock<std::sync::Mutex<Option<Arc<PluginExecutor>>>> =
	std::sync::OnceLock::new();

fn executor_slot() -> &'static std::sync::Mutex<Option<Arc<PluginExecutor>>> {
	PLUGIN_EXECUTOR.get_or_init(|| std::sync::Mutex::new(None))
}

/// Install the plugin job executor (quercusplugin registration point;
/// `None` clears it).
pub fn set_plugin_executor(executor: Option<Arc<PluginExecutor>>) {
	*executor_slot().lock().unwrap_or_else(|e| e.into_inner()) = executor;
}

/// The installed plugin executor, if any.
///
/// Returns a clone of the executor installed by [`set_plugin_executor`], or
/// `None` when none is installed (the plugin render path then falls back to
/// a purple failure frame). The returned `Arc` keeps the executor alive even
/// if a later install clears or replaces the slot. Takes the slot lock and
/// recovers from a poisoned mutex instead of panicking.
pub fn plugin_executor() -> Option<Arc<PluginExecutor>> {
	executor_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clone()
}

/// Plugin instance factory: resolves an OFX plugin identifier to a live
/// instance-registry id, creating (and caching) the instance lazily.
/// Implemented by the quercusplugin crate — the montage effect path carries
/// plugin identifiers, not instance ids (the montage is resolved from
/// the timeline in the main process; the instance lives in whichever
/// process renders the frame).
pub type PluginInstanceFactory = dyn Fn(&str) -> Option<u64> + Send + Sync;

static PLUGIN_INSTANCE_FACTORY: std::sync::OnceLock<
	std::sync::Mutex<Option<Arc<PluginInstanceFactory>>>,
> = std::sync::OnceLock::new();

fn instance_factory_slot() -> &'static std::sync::Mutex<Option<Arc<PluginInstanceFactory>>> {
	PLUGIN_INSTANCE_FACTORY.get_or_init(|| std::sync::Mutex::new(None))
}

/// Install the plugin instance factory (quercusplugin registration point;
/// `None` clears it).
pub fn set_plugin_instance_factory(factory: Option<Arc<PluginInstanceFactory>>) {
	*instance_factory_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner()) = factory;
}

/// The installed plugin instance factory, if any.
pub fn plugin_instance_factory() -> Option<Arc<PluginInstanceFactory>> {
	instance_factory_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clone()
}

/// Whether a plugin identifier can render on the GPU/GL path. Montage
/// tickets use this to keep CPU-only OFX plugins on the CPU compositor while
/// GPU-capable plugins stay in the GPU montage. Implemented by
/// quercusplugin from its descriptor registry.
pub type PluginGpuQuery = dyn Fn(&str) -> bool + Send + Sync;

static PLUGIN_GPU_QUERY: std::sync::OnceLock<std::sync::Mutex<Option<Arc<PluginGpuQuery>>>> =
	std::sync::OnceLock::new();

fn plugin_gpu_query_slot() -> &'static std::sync::Mutex<Option<Arc<PluginGpuQuery>>> {
	PLUGIN_GPU_QUERY.get_or_init(|| std::sync::Mutex::new(None))
}

/// Install/clear the plugin GPU-capability query.
///
/// The query maps an OFX plugin identifier to whether it can render on the
/// GPU path; [`montage_has_cpu_effects`] consults it through
/// [`plugin_is_gpu`] to keep CPU-only plugins on the CPU compositor (and to
/// let GPU-capable plugins stay in the GPU montage). Passing `None` clears
/// the process-wide slot, after which every non-builtin effect is
/// conservatively treated as CPU. Safe to call repeatedly; the previous
/// `Arc`, if any, is dropped.
pub fn set_plugin_gpu_query(query: Option<Arc<PluginGpuQuery>>) {
	*plugin_gpu_query_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner()) = query;
}

fn plugin_is_gpu(id: &str) -> bool {
	plugin_gpu_query_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.as_ref()
		.is_some_and(|query| query(id))
}

/// A montage is CPU-bound when it contains a CPU-only OFX plugin. Built-in
/// Opacity is GPU-rendered; a GPU-capable OFX plugin stays in the GPU
/// montage. Without a capability query (plugin subsystem not installed), a
/// non-builtin effect is conservatively treated as CPU.
fn montage_has_cpu_effects(params: &crate::ticket::VideoTicketParams) -> bool {
	let cpu_effect = |effect: &crate::ticket::MontageEffect| {
		effect.enabled
			&& effect.type_id != OPACITY_EFFECT_TYPE_ID
			&& !plugin_is_gpu(&effect.type_id)
	};
	params
		.montage
		.iter()
		.any(|clip| clip.effects.iter().any(cpu_effect))
		|| params
			.adjustments
			.iter()
			.any(|span| span.effects.iter().any(cpu_effect))
}

/// Box a resolved texture into the table's texture channel (the render
/// seam's output is the one place a texture legitimately travels as a
/// refcounted handle — [`quercus_node::handle::get_checked`] probes against
/// `Texture` stay type-checked).
fn texture_value(texture: Texture) -> NodeValue {
	NodeValue::Texture(quercus_node::handle::make_owned(texture))
}

/// The failure marker frame: solid magenta (1, 0, 1, 1) F32 RGBA —
/// the C++ plugin renderer paints failed plugin output purple so a
/// broken plugin is visible instead of silently black.
///
/// `size` is clamped to at least 1×1 (a zero/negative dimension becomes 1);
/// `time` is used as the generated frame's timestamp. Every pixel is set to
/// exactly `(1.0, 0.0, 1.0, 1.0)` F32 RGBA. Falls back to
/// [`Texture::dummy`] when the frame cannot be allocated, so the caller
/// never has to handle an error.
fn purple_frame(time: Rational, size: (i32, i32)) -> Texture {
	let (w, h) = (size.0.max(1), size.1.max(1));
	let mut frame = match generate_frame(time, (w, h), PixelFormat::F32) {
		Ok(f) => f,
		Err(_) => return Texture::dummy(),
	};
	for pixel in frame.data.chunks_exact_mut(16) {
		for (i, v) in [1.0f32, 0.0, 1.0, 1.0].iter().enumerate() {
			pixel[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
		}
	}
	Texture::wrap_frame(frame)
}

impl quercus_node::traverser::RenderHooks for RenderEvalHooks {
	fn use_cache(&self) -> bool {
		self.use_cache
	}

	fn is_cancelled(&self) -> bool {
		// TODO(phase-6b): poll the ticket's cancellation flag here so
		// long plugin renders can be interrupted.
		false
	}

	fn resolve(
		&mut self,
		node: quercus_node::id::NodeId,
		_row: &NodeValueRow,
		table: &mut NodeValueTable,
	) {
		let _ = node;
		// One pass over the table: every texture-channel value that boxes
		// a job resolves in place (jobs nested in its inputs first). A
		// job that produces no value leaves its box, so a later row
		// probing the same box still sees the unresolved request.
		let mut in_flight = HashSet::new();
		for (_, value, _) in table.rows_mut() {
			self.resolve_value(value, 0, &mut in_flight);
		}
	}
}

impl Default for RenderEvalHooks {
	fn default() -> Self {
		Self::new()
	}
}

/// Generate the pipeline's canonical frame: F32 RGBA, transparent black,
/// with the given timestamp (the CPU-backend producer for video tickets).
///
/// `size` is `(width, height)` in pixels; `time` is stored as the frame
/// timestamp; `format` is written into the frame's video params (F32 is
/// the pipeline's canonical format). The frame is allocated and zeroed;
/// callers fill the pixels.
///
/// # Errors
///
/// `Error::Invalid` for a non-positive dimension and `Error::NoMem` when
/// the allocation fails.
pub fn generate_frame(time: Rational, size: (i32, i32), format: PixelFormat) -> Result<Frame> {
	let (w, h) = size;
	if w <= 0 || h <= 0 {
		return Err(Error::Invalid);
	}
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = w;
	pod.height = h;
	pod.format = format as i32;
	frame.set_video_params(pod);
	frame.timestamp = time;
	if !frame.allocate() {
		return Err(Error::NoMem);
	}
	Ok(frame)
}

/// The manager-installed ticket producer: render the frame the ticket
/// asks for (F32 pipeline frame). This is the CPU-backend render path.
///
/// M12 P0 routing: a sequence montage (list of clips) is composited
/// topmost-last through [`render_montage_frame`], which prefers the GPU
/// compositor; a single-footage ticket decodes one stream through
/// [`render_footage_frame`]; otherwise a transparent generated frame is
/// produced — a GPU clear when the format is F32 and a context exists,
/// falling back to [`generate_frame`].
///
/// `time` is the sequence time to render; `params` supplies the output
/// size ([`VideoTicketParams::render_size`]), the optional forced pixel
/// format (F32 when unset) and the clip/footage selection.
///
/// # Errors
///
/// Propagates decode and composite errors, and `Error::Invalid`/
/// `Error::NoMem` from the generated-frame fallback.
pub fn render_produced_frame(
	time: Rational,
	params: &crate::ticket::VideoTicketParams,
) -> Result<Texture> {
	let (w, h) = params.render_size();
	let format = params.force_format.unwrap_or(PixelFormat::F32);

	if !params.montage.is_empty() {
		let r = render_montage_frame(time, params, (w, h), format);
		return r.map(|texture| texture.with_timestamp(time));
	}
	if let Some((filename, stream_index)) = &params.footage {
		return render_footage_frame(filename, *stream_index, time, (w, h), format)
			.map(|texture| texture.with_timestamp(time));
	}

	// Generated (transparent) frame: prefer a GPU clear so the pipeline
	// stays GPU end to end (M2); fall back to the CPU producer.
	if format == PixelFormat::F32 {
		if let Some(ctx) = quercus_core::backend::GpuContext::shared() {
			if let Ok(token) = ctx.create_texture(w, h) {
				if ctx.clear_texture(token).is_ok() {
					return Ok(
						Texture::gpu(ctx, token, w, h, PixelFormat::F32).with_timestamp(time)
					);
				}
				ctx.destroy_texture(token);
			}
		}
	}
	let frame = generate_frame(time, (w, h), format)?;
	Ok(Texture::wrap_frame(frame))
}

// ---------------------------------------------------------------------------
// Footage decode (M12 P0): the quercuscodec bridge
// ---------------------------------------------------------------------------

/// Process-wide open decoder sessions, keyed by (filename, stream).
/// Sessions are mutex-serialized inside the quercuscodec box, so sharing
/// one handle across worker threads is safe. The value carries an LRU
/// tick: the map is capped ([`MAX_CACHED_DECODERS`]) because every
/// session pins an FFmpeg context plus up to two native decoded frames
/// (~50 MB at 4K) — before the cap, scrubbing a footage bin grew the
/// map without bound.
static DECODERS: std::sync::OnceLock<
	std::sync::Mutex<
		std::collections::HashMap<(String, i32), (Arc<dyn quercus_codec::decoder::Decoder>, u64)>,
	>,
> = std::sync::OnceLock::new();

/// Cap on cached decoder sessions per process (LRU beyond this). 16
/// covers heavy multi-clip montages without reopen thrash; eviction only
/// drops the map entry — an in-flight render keeps its Arc alive and the
/// session dies with the last reference (Drop releases FFmpeg).
///
/// Eviction is hardware-first: hardware sessions pin GPU memory (each
/// NVDEC decoder holds a surface pool — ~100 MB at 4K), so a full cache
/// with live hardware sessions can exhaust the GPU's video memory and
/// make the NEXT decoder open fail with `cuvidCreateDecoder` OOM (the
/// "4K 切换后大量 CUDA_ERROR_OUT_OF_MEMORY 报错" log flood). Software
/// sessions (system RAM only) are evicted only when nothing else is
/// available.
const MAX_CACHED_DECODERS: usize = 6;

/// LRU tick source for [`DECODERS`].
static DECODER_TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Count of [`render_footage_frame_inner_opts`] entries, i.e. of real codec work
/// on the footage path (frame-cache hits and decode-service LRU hits do not
/// count). Visible for the decode-service tests, which use it to tell a
/// cached frame from a re-decode.
static DECODE_INVOCATIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The number of real footage decodes since the last
/// [`reset_decode_invocations`] (or process start).
pub fn decode_invocations() -> u64 {
	DECODE_INVOCATIONS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reset the [`decode_invocations`] counter to zero.
pub fn reset_decode_invocations() {
	DECODE_INVOCATIONS.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Cap on decoded FRAMES cached per process (release builds of the graph
/// renderer re-decode every frame the pre-render window pulls — each is a
/// seek+decode on the shared decoder session, which serializes the graph
/// path behind the montage baseline. A small frame LRU lets playback pull
/// the forward window from cache instead of thrashing the decoder: the
/// window is sequential, so a short FIFO of recently decoded frames hits
/// on every pre-render restart and on repeat plays).
const MAX_CACHED_FRAMES: usize = 24;

/// Cap on cached PLANAR hardware frames (M5 audit): each planar entry
/// pins a decoder surface through its keep-alive guard, and the driver's
/// surface pool is finite. Planar textures only need to bridge the decode
/// thread → resolve, so they get a much smaller budget than the general
/// frame LRU. Eviction drops only the import; the decoder session cache
/// still holds the raw frame, so a re-request re-imports (no re-decode).
const MAX_CACHED_PLANAR_FRAMES: usize = 4;

/// Decoded-frame LRU key: `(filename, stream, time, w, h)`. The size is
/// part of the key: the same media at a different target resolution is a
/// different frame (an interleaved source-monitor/proxy request must not
/// reuse a wrongly-sized pixel buffer).
type DecodedFrameKey = (String, i32, (i64, i64), i32, i32, bool);

/// Decoded-frame LRU map. Values are decoded textures (CPU frame, or the
/// M5 imported planar texture when the hardware import took the frame)
/// plus their LRU tick. Frame data is the expensive part (a 1080p F32
/// frame ≈ 31 MB); the decoder session cache alone still re-decodes every
/// `render_footage_frame`.
type DecodedFrameCache = std::collections::HashMap<DecodedFrameKey, (Texture, u64)>;

static DECODED_FRAMES: std::sync::OnceLock<std::sync::Mutex<DecodedFrameCache>> =
	std::sync::OnceLock::new();

fn decoded_frames() -> std::sync::MutexGuard<'static, DecodedFrameCache> {
	DECODED_FRAMES
		.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
		.lock()
		.unwrap_or_else(|e| e.into_inner())
}

/// Insert `texture` under `key` with the general and planar caps applied.
///
/// A key that is already present is left untouched (the first insertion
/// wins). Otherwise general-LRU victims are dropped until the cache is
/// below [`MAX_CACHED_FRAMES`]; entries with tick 0 are pinned and never
/// chosen as victims, so a cache full of them simply grows past the cap.
/// A planar texture additionally prunes planar entries to
/// [`MAX_CACHED_PLANAR_FRAMES`] - 1 before inserting, keeping hardware
/// surface residency bounded. `tick` is the caller's LRU timestamp (higher
/// is more recent); `cache` is borrowed for the whole operation.
fn insert_cached_frame(
	cache: &mut DecodedFrameCache,
	key: DecodedFrameKey,
	texture: Texture,
	tick: u64,
) {
	if cache.contains_key(&key) {
		return;
	}
	while cache.len() >= MAX_CACHED_FRAMES {
		let Some(victim) = cache
			.iter()
			.filter(|(_, (_, t))| *t > 0)
			.min_by_key(|(_, (_, t))| *t)
			.map(|(k, _)| k.clone())
		else {
			break;
		};
		cache.remove(&victim);
	}
	if texture.is_planar() {
		prune_planar_frames(cache, MAX_CACHED_PLANAR_FRAMES.saturating_sub(1));
	}
	cache.insert(key, (texture, tick));
}

/// Evict the least-recently-used planar entries until at most `keep`
/// remain (hardware-surface residency bound, M5 audit).
fn prune_planar_frames(cache: &mut DecodedFrameCache, keep: usize) {
	let mut planar: Vec<(DecodedFrameKey, u64)> = cache
		.iter()
		.filter(|(_, (t, _))| t.is_planar())
		.map(|(k, (_, tick))| (k.clone(), *tick))
		.collect();
	if planar.len() <= keep {
		return;
	}
	planar.sort_by_key(|(_, tick)| *tick);
	for (key, _) in planar.drain(..planar.len() - keep) {
		cache.remove(&key);
	}
}

/// The deterministic frame-LRU key half for a time value.
///
/// Returns the rational as `(numerator, denominator)` — the exact pair, not
/// a float — so two requests for the same frame time compare equal even
/// when reached through different rational arithmetic. `t` is borrowed and
/// not reduced or normalized. Used as the time component of
/// [`DecodedFrameKey`].
fn time_key(t: &Rational) -> (i64, i64) {
	(t.numerator(), t.denominator())
}

fn decoders() -> std::sync::MutexGuard<
	'static,
	std::collections::HashMap<(String, i32), (Arc<dyn quercus_codec::decoder::Decoder>, u64)>,
> {
	DECODERS
		.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
		.lock()
		.unwrap_or_else(|e| e.into_inner())
}

/// Pick the victim to evict under the LRU cap: the least-recently-used
/// HARDWARE session when one exists (freeing GPU video memory first,
/// which is the scarce resource), otherwise the least-recently-used
/// session of any kind.
fn eviction_victim(
	cache: &std::collections::HashMap<
		(String, i32),
		(Arc<dyn quercus_codec::decoder::Decoder>, u64),
	>,
) -> Option<(String, i32)> {
	cache
		.iter()
		.filter(|(_, (decoder, _))| decoder.hardware_decoding())
		.min_by_key(|(_, (_, t))| *t)
		.map(|(k, _)| k.clone())
		.or_else(|| {
			cache
				.iter()
				.min_by_key(|(_, (_, t))| *t)
				.map(|(k, _)| k.clone())
		})
}

/// Open (or reuse) the decoder session for `(filename, stream_index)`.
///
/// Sessions are cached process-wide in [`DECODERS`], mutex-serialized so a
/// handle can be shared across worker threads, and a cache hit refreshes
/// the session's LRU tick. A miss opens a new FFmpeg decoder and evicts
/// under [`MAX_CACHED_DECODERS`], preferring a hardware session as the
/// victim (hardware sessions pin scarce GPU memory). `filename` is the
/// media path (`&str`, borrowed only for the duration of the call) and
/// `stream_index` selects the stream inside that file.
///
/// Setting the `QUERCUS_PERF` environment variable adds `[open]` tracing
/// to standard error.
///
/// # Errors
///
/// `Error::Failed` when the codec cannot open the stream.
fn open_decoder(
	filename: &str,
	stream_index: i32,
) -> Result<Arc<dyn quercus_codec::decoder::Decoder>> {
	let key = (filename.to_string(), stream_index);
	let tick = DECODER_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	if std::env::var_os("QUERCUS_PERF").is_some() {
		eprintln!("[open] {:?} s{} (request)", filename, stream_index);
	}
	{
		let mut cache = decoders();
		if let Some((d, t)) = cache.get_mut(&key) {
			*t = tick;
			if std::env::var_os("QUERCUS_PERF").is_some() {
				eprintln!("[open] {:?} s{} CACHED", filename, stream_index);
			}
			return Ok(d.clone());
		}
	}
	// Hardware-session budget is now handled by the LRU cap below (and the
	// GPU-vram worker-count policy): the "evict the ONLY hardware session
	// before every new open" guard below was eviction-on-every-frame — a
	// live session was dropped before the NEXT request for the same file
	// could hit it, so every frame re-opened the decoder (~0.5 s each) and
	// playback could never keep up (the [open] (request) log flood without
	// a single CACHED hit).
	let decoder: Arc<dyn quercus_codec::decoder::Decoder> = Arc::new(FFmpegDecoder::new());
	let stream = CodecStream::with_block(filename.to_string(), stream_index, None);
	decoder
		.open(&stream)
		.map_err(|e| Error::Failed(format!("footage decode open: {e:?}")))?;
	let mut cache = decoders();
	// LRU eviction: hardware-first (free the GPU memory a full cache of
	// hardware sessions pins — the scarcity that breaks the next open);
	// the victim is dropped here, but an in-flight render holds its own
	// Arc — the FFmpeg context (and its GPU surface pool) dies with the
	// last reference.
	while cache.len() >= MAX_CACHED_DECODERS {
		let Some(victim) = eviction_victim(&cache) else {
			break;
		};
		cache.remove(&victim);
	}
	cache.insert(key, (decoder.clone(), tick));
	Ok(decoder)
}

/// Decode the footage frame at `time` and copy/scale it into an
/// quercusrender F32 frame of `(w, h)`.
///
/// While a [`crate::pipeline::DecodeService`] is installed (the pipeline
/// backend's decode thread) the decode is a rendezvous with that service,
/// which caches frames and keeps the codec calls off the caller's thread;
/// with no service installed this is the synchronous decode it has always
/// been. Either way the pixels are the same.
pub fn render_footage_frame(
	filename: &str,
	stream_index: i32,
	time: Rational,
	size: (i32, i32),
	format: PixelFormat,
) -> Result<Texture> {
	let started = std::time::Instant::now();
	let decode = |allow_import: bool| -> Result<Texture> {
		match crate::pipeline::decode_service() {
			Some(service) => {
				let request = crate::pipeline::DecodeRequest {
					filename: filename.to_string(),
					stream_index,
					time,
					size,
					format,
					allow_import: true,
				};
				match service.request(request) {
					Some(result) => result,
					// The service went away (shutdown) mid-flight: decode here
					// rather than failing the frame.
					None => render_footage_frame_inner_opts(
						filename,
						stream_index,
						time,
						size,
						format,
						allow_import,
					),
				}
			}
			None => render_footage_frame_inner_opts(
				filename,
				stream_index,
				time,
				size,
				format,
				allow_import,
			),
		}
	};
	let mut result = decode(true);
	// M5: an imported hardware frame is planar YUV; resolve it to
	// working-space RGBA on the GPU before it leaves the footage path.
	// The resolve scales straight to the caller's target size on the GPU
	// (no CPU swscale / no native-size RGBA intermediate), which is what
	// keeps a 4K clip on the GPU for a preview-size montage frame.
	// A resolution failure is a per-frame fallback: re-decode through the
	// CPU scaler with the import disabled (the decoder session cache makes
	// this a transfer, not a second decode).
	if let Ok(texture) = &result {
		if texture.is_planar() {
			let target = (size.0 > 0 && size.1 > 0).then_some(size);
			result = match resolve_planar_footage(texture, target) {
				Ok(resolved) => Ok(resolved),
				Err(err) => {
					eprintln!("planar footage resolve failed, staging fallback: {err:#}");
					// Bypass the decode service: its cache still holds the
					// planar entry that just failed to resolve.
					render_footage_frame_inner_opts(
						filename,
						stream_index,
						time,
						size,
						format,
						false,
					)
				}
			};
		}
	}
	if std::env::var_os("QUERCUS_PERF").is_some() {
		eprintln!(
			"[decode] {:?} s{} time {}/{} ({:.3}s) size {:?} -> {:.3}s {:?}",
			filename,
			stream_index,
			time.numerator(),
			time.denominator(),
			time.to_f64(),
			size,
			started.elapsed().as_secs_f64(),
			result.is_ok()
		);
	}
	result
}

/// The CPU-staging decode for consumers that composite on the CPU (the
/// worker/no-adapter montage fallback): same service/FRU semantics as
/// [`render_footage_frame`], but the M5 zero-copy import is disabled and
/// the result is always a CPU frame. The GPU montage compositor
/// ([`render_montage_frame`]) uses [`render_footage_frame`] instead.
///
/// This is not just an optimization: the CPU montage compositor runs on
/// the CPU, so an imported `Texture::Gpu` would have to be downloaded
/// again — and before this entry point existed it was silently SKIPPED
/// (the M5 audit's all-black montage bug). A planar texture must never
/// reach a CPU consumer here.
pub(crate) fn render_footage_frame_staged(
	filename: &str,
	stream_index: i32,
	time: Rational,
	size: (i32, i32),
	format: PixelFormat,
) -> Result<Texture> {
	let result = match crate::pipeline::decode_service() {
		Some(service) => {
			let request = crate::pipeline::DecodeRequest {
				filename: filename.to_string(),
				stream_index,
				time,
				size,
				format,
				allow_import: false,
			};
			match service.request(request) {
				Some(result) => result,
				None => render_footage_frame_inner_opts(
					filename,
					stream_index,
					time,
					size,
					format,
					false,
				),
			}
		}
		None => render_footage_frame_inner_opts(filename, stream_index, time, size, format, false),
	};
	// Defensive: a planar texture (a stale service entry from an older
	// key layout) can never satisfy a staging request — re-decode CPU.
	match result {
		Ok(texture) if texture.is_planar() => {
			render_footage_frame_inner_opts(filename, stream_index, time, size, format, false)
		}
		other => other,
	}
}

/// The synchronous decode behind [`render_footage_frame`] (frame-LRU →
/// decoder session → codec → F32 frame). `pub(crate)` because the decode
/// service runs exactly this on its own thread, with the request's own
/// `allow_import` flag (the CPU montage fallback requests staging on
/// purpose, §M5 audit).
///
/// `allow_import` false forces the CPU staging path: the resolve step's
/// fallback re-decodes with it off, and the CPU montage compositor never
/// wants a GPU frame.
pub(crate) fn render_footage_frame_inner_opts(
	filename: &str,
	stream_index: i32,
	time: Rational,
	size: (i32, i32),
	format: PixelFormat,
	allow_import: bool,
) -> Result<Texture> {
	// (file, stream, time) repeatedly (pre-render restarts, repeated
	// scale-up at the same time, graph + montage interleaving); the
	// decode itself is the expensive part and must not re-run per
	// request. The target size is part of the key (see the cache
	// type's doc).
	let (w, h) = size;
	let cache_size = if w > 0 && h > 0 { (w, h) } else { (-1, -1) };
	let cache_key = (
		filename.to_string(),
		stream_index,
		time_key(&time),
		cache_size.0,
		cache_size.1,
		allow_import,
	);
	{
		let mut cache = decoded_frames();
		let tick = DECODER_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		let texture = cache.get(&cache_key).map(|(f, _)| f.clone());
		if let Some(texture) = texture {
			// A planar entry cannot satisfy a staging request (the resolve
			// step failed and asked for CPU pixels): treat it as a miss and
			// decode through the scaler instead.
			if allow_import || !texture.is_planar() {
				cache.insert(cache_key.clone(), (texture.clone(), tick));
				return Ok(texture);
			}
			cache.remove(&cache_key);
		}
	}
	// Past the frame cache: this call really goes to the codec (the
	// decode-service tests read this counter to prove it).
	DECODE_INVOCATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	let decoder = open_decoder(filename, stream_index)?;
	let params = RetrieveVideoParams {
		stream: CodecStream::with_block(filename.to_string(), stream_index, None),
		time,
		length: TimeRange::default(),
		force_range: K_COLOR_RANGE_DEFAULT,
		is_image_sequence: false,
		image_sequence_digits: 0,
		image_sequence_number: 0,
		mode: RenderMode::Offline,
		alpha_is_premultiplied: false,
		// 直接按目标尺寸出帧：swscale 一次完成格式转换 + 缩放，不再
		// 产生全分辨率 F32 中间帧（4K 预览每帧省 ~260MB 瞬时拷贝）。
		target_size: if w > 0 && h > 0 {
			Some((w as u32, h as u32))
		} else {
			None
		},
	};

	// M5: hardware frames are offered to the zero-copy import first; the
	// CPU retrieval below is the per-frame staging fallback (the decoder
	// session cache holds the raw surface, so the fallback never
	// re-decodes).
	if allow_import {
		if let Ok(Some(imported)) = decoder.retrieve_video_frame_gpu(&params) {
			// Imported planar frames are transient: the render layer resolves
			// them immediately and returns the GPU result. Do not cache the
			// planar texture here — its VMM slot must be released (after the
			// GPU pass completes) so the slot can be reused next frame.
			return Ok(imported);
		}
	}

	let decoded = decoder
		.retrieve_video_frame(&params)
		.map_err(|e| Error::Failed(format!("footage decode at {time:?}: {e:?}")))?;

	let src_w = decoded.width();
	let src_h = decoded.height();
	let src_linesize = decoded.linesize_bytes();
	if src_w <= 0 || src_h <= 0 || src_linesize <= 0 || !decoded.is_allocated() {
		return Err(Error::Failed("footage decode: bad decoded frame".into()));
	}

	// `(0, 0)` means "native size": decode without scaling and produce a
	// frame matching the decoded dimensions (M12: the graph sequence
	// path requests native frames and scales at composite time).
	let (dw, dh) = if w > 0 && h > 0 {
		(w, h)
	} else {
		(src_w, src_h)
	};
	let mut dst = generate_frame(time, (dw, dh), format)?;
	let dst_linesize = dst.linesize_bytes() as i32;
	let src_data = match decoded.data() {
		Some(d) => d,
		None => return Err(Error::Failed("footage decode: no frame data".into())),
	};

	if src_w == dw && src_h == dh && src_linesize == dst_linesize {
		let bytes = (src_h as usize)
			.checked_mul(src_linesize as usize)
			.ok_or(Error::NoMem)?;
		dst.data[..bytes].copy_from_slice(&src_data[..bytes]);
	} else {
		scale_rgba_f32(
			src_data.as_ptr(),
			src_linesize,
			src_w,
			src_h,
			&mut dst.data,
			dst_linesize,
			dw,
			dh,
		);
	}
	// Input node: source colorspace → the pipeline working space (ACEScg
	// by default; the legacy sRGB working space keeps the pass-through).
	convert_decoded_to_working(&mut dst, &decoded);
	// Memoize the finished working-space frame (LRU-capped).
	let texture = Texture::wrap_frame(dst);
	{
		let mut cache = decoded_frames();
		let tick = DECODER_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		insert_cached_frame(&mut cache, cache_key, texture.clone(), tick);
	}
	Ok(texture)
}

/// Resolve an imported planar frame (M5) to working-space RGBA on the
/// GPU: the descriptor-driven planar→RGB pass (matrix/range from the
/// frame's own colorimetry; any color model, subsampling and storage —
/// YUV 4:2:0/4:2:2/4:4:4 and RGB/RGBA, U8/U16/F16/F32) followed by the
/// source→working transform. `target` is the output size the caller asked
/// for (the ticket/preview size): when it differs from the frame's
/// visible luma size the pass scales with normalized-UV bilinear on the
/// GPU, so a native 4K frame never needs a CPU swscale or a 4K RGBA
/// intermediate. `None`/non-positive keeps the native visible size.
///
/// In the legacy sRGB working space the source→working transform is a
/// pass-through (matching the CPU path's `convert_decoded_to_working`);
/// otherwise it is applied as a CPU reference-baked 3D LUT, exactly like
/// the OCIO color transforms.
fn resolve_planar_footage(texture: &Texture, target: Option<(i32, i32)>) -> Result<Texture> {
	let planar = texture
		.as_planar()
		.ok_or_else(|| Error::Failed("resolve_planar_footage: not planar".into()))?;
	let Some(gpu) = planar
		.ctx
		.as_any()
		.and_then(|a| a.downcast_ref::<quercus_core::backend::GpuContext>())
	else {
		return Err(Error::Failed(
			"planar texture belongs to a non-wgpu context".into(),
		));
	};
	// The visible luma size, not the (possibly padded) plane allocation:
	// the matrix pass samples only this region and scales it to the
	// target. Any target size/aspect is valid — the mapping is fill/stretch
	// (the same behavior the CPU path gets from swscale `target_size`).
	let src_w = planar.width.max(1);
	let src_h = planar.height.max(1);
	let (w, h) = match target {
		Some((w, h)) if w > 0 && h > 0 => (w, h),
		_ => (src_w, src_h),
	};
	let dst = gpu
		.create_texture(w, h)
		.map_err(|e| Error::Failed(format!("planar resolve target: {e:?}")))?;
	if std::env::var_os("OAK_DEBUG_HW").is_some() {
		eprintln!("[planar] resolve target created");
	}
	if let Err(e) = gpu.run_planar_matrix_to_rgb(
		planar.plane_tokens(),
		dst,
		planar.descriptor,
		&planar.transform,
		(src_w as u32, src_h as u32),
		(w as u32, h as u32),
	) {
		gpu.destroy_texture(dst);
		return Err(Error::Failed(format!("planar matrix pass: {e:?}")));
	}
	if std::env::var_os("OAK_DEBUG_HW").is_some() {
		eprintln!("[planar] matrix pass submitted");
	}
	let token = match footage_working_lut(planar.color_primaries, planar.color_trc) {
		Some((key, lut)) => match gpu.apply_color_lut(dst, &key, &lut) {
			Ok(transformed) => {
				gpu.destroy_texture(dst);
				transformed
			}
			Err(e) => {
				gpu.destroy_texture(dst);
				return Err(Error::Failed(format!(
					"footage working-space LUT failed: {e:?}"
				)));
			}
		},
		None => dst,
	};
	// Hold the VMM/decode-surface guard on the resolved token until the
	// token (including any async readback) is destroyed. Without this the
	// source slot can be reused while the GPU still reads the resolved
	// frame in the async pipeline.
	if let Some(guard) = planar.keep_alive.clone() {
		gpu.set_texture_keep_alive(token, guard);
	}
	Ok(Texture::gpu(
		planar.ctx.clone(),
		token,
		w,
		h,
		PixelFormat::F32,
	))
}

/// The source→working-space 3D LUT for a decoded frame (M5 GPU import
/// path): baked from the same CPU reference (`colormath::decode_to_acescg`)
/// the staging path uses, cached per colorimetry. `None` in the legacy
/// sRGB working space (pass-through) or when the LUT cannot be built.
///
/// `color_primaries`/`color_trc` are the frame's `AVColorPrimaries` and
/// `AVColorTransferCharacteristic` codes. The returned key
/// (`"footage/{primaries}/{trc}"`) is passed to the GPU LUT pass as its
/// cache identity; the per-process cache holds at most 16 entries and is
/// cleared wholesale when full.
fn footage_working_lut(
	color_primaries: i32,
	color_trc: i32,
) -> Option<(String, std::sync::Arc<quercus_core::lut::Lut3d>)> {
	use quercus_core::colormath::WorkingColorSpace;
	if quercus_core::color::pipeline_working_space() == WorkingColorSpace::SrgbLegacy {
		return None;
	}
	let key = format!("footage/{color_primaries}/{color_trc}");
	type Cache = std::sync::Mutex<
		std::collections::HashMap<String, std::sync::Arc<quercus_core::lut::Lut3d>>,
	>;
	static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
	let mut cache = CACHE
		.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	if let Some(lut) = cache.get(&key) {
		return Some((key, lut.clone()));
	}
	let lut = build_footage_working_lut(color_primaries, color_trc)?;
	if cache.len() >= 16 {
		cache.clear();
	}
	cache.insert(key.clone(), lut.clone());
	Some((key, lut))
}

/// Bake the source→ACEScg transform into a 3D LUT over the display
/// domain (the same domain the color-transform LUTs use).
///
/// `color_primaries` and `color_trc` are the decoded frame's FFmpeg
/// `AVColorPrimaries` / `AVColorTransferCharacteristic` integer codes,
/// mapped through [`source_primaries_from_av`] / [`source_transfer_from_av`];
/// every lattice point is transformed with the same CPU reference
/// ([`decode_to_acescg`]) the staging path uses. The returned LUT is the
/// GPU-side equivalent for imported planar frames.
///
/// Returns `None` when the transform cannot be produced; the current
/// implementation always returns `Some`, so callers treat `None` as
/// "working-space transform unavailable".
fn build_footage_working_lut(
	color_primaries: i32,
	color_trc: i32,
) -> Option<std::sync::Arc<quercus_core::lut::Lut3d>> {
	use quercus_core::colormath::{
		decode_to_acescg, source_primaries_from_av, source_transfer_from_av,
	};
	use quercus_core::lut::Lut3d;
	let edge = Lut3d::DISPLAY_EDGE;
	let (lo, hi) = (Lut3d::DISPLAY_LO, Lut3d::DISPLAY_HI);
	let n = (edge as usize).pow(3);
	let mut samples = vec![0.0f32; n * 4];
	let step = |i: usize, axis: usize| -> f32 {
		let t = i as f32 / (edge - 1) as f32;
		lo[axis] + (hi[axis] - lo[axis]) * t
	};
	for b in 0..edge as usize {
		for g in 0..edge as usize {
			for r in 0..edge as usize {
				let idx = ((b * edge as usize + g) * edge as usize + r) * 4;
				samples[idx] = step(r, 0);
				samples[idx + 1] = step(g, 1);
				samples[idx + 2] = step(b, 2);
				samples[idx + 3] = 1.0;
			}
		}
	}
	decode_to_acescg(
		&mut samples,
		source_primaries_from_av(color_primaries),
		source_transfer_from_av(color_trc),
	);
	let mut data = Vec::with_capacity(n * 3);
	for px in samples.chunks_exact(4) {
		data.extend_from_slice(&px[..3]);
	}
	Some(std::sync::Arc::new(Lut3d { edge, lo, hi, data }))
}

/// Convert a decoded footage frame (display-referred RGB in the source's
/// own colorspace) into the pipeline working space, driven by the frame's
/// colorimetry metadata (carried on the codec frame's params). A no-op in
/// the legacy sRGB working space or when the frame has no pixel data.
fn convert_decoded_to_working(dst: &mut Frame, decoded: &quercus_codec::frame::Frame) {
	use quercus_core::colormath::{
		source_primaries_from_av, source_transfer_from_av, WorkingColorSpace,
	};
	if quercus_core::color::pipeline_working_space() == WorkingColorSpace::SrgbLegacy {
		return;
	}
	// Frames without colorimetry metadata get the generic fallback (sRGB
	// primaries, sRGB transfer) instead of passing through unconverted;
	// the missing tag is warned once per process.
	let (primaries, transfer) = match decoded.params() {
		Some(params) => (
			source_primaries_from_av(params.color_primaries()),
			source_transfer_from_av(params.color_transfer()),
		),
		None => {
			warn_missing_colorimetry_once();
			(source_primaries_from_av(2), source_transfer_from_av(2))
		}
	};
	let w = dst.width.max(0) as usize;
	let h = dst.height.max(0) as usize;
	if w == 0 || h == 0 {
		return;
	}
	let row_bytes = w * 16; // F32 RGBA
	let linesize = dst.linesize_bytes();
	quercus_core::cpuparallel::for_each_row(&mut dst.data, linesize, h, w, |_, row| {
		if row.len() >= row_bytes {
			quercus_core::colormath::decode_to_acescg_bytes(
				&mut row[..row_bytes],
				w,
				primaries,
				transfer,
			);
		}
	});
}

// ---------------------------------------------------------------------------
// Graph-driven sequence rendering
// ---------------------------------------------------------------------------

/// WGSL fragment for the graph compositor's alpha-over pass (the C++
/// viewer shader is `:/shaders/alphaover.frag`; same premultiplied-over
/// math on raw texture loads). Bindings: 1 = destination (accumulator),
/// 3 = source (the clip frame) — the layout [`GpuContext::compile_shader_pass`]
/// assigns to texture pairs.
const COMP_WGSL: &str = r#"
@group(0) @binding(1) var dst_tex: texture_2d<f32>;
@group(0) @binding(3) var src_tex: texture_2d<f32>;

@fragment
fn main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = textureDimensions(dst_tex);
    let coord = clamp(vec2<u32>(u32(i32(frag.x)), u32(i32(frag.y))), vec2<u32>(0u, 0u), dims - vec2<u32>(1u, 1u));
    let s = textureLoad(src_tex, coord, 0);
    let d = textureLoad(dst_tex, coord, 0);
    let a = clamp(s.a, 0.0, 1.0);
    // RGB keeps the working-space values unclamped (HDR/WCG can exceed
    // 1.0); only the alpha of the result is clamped to the valid range.
    return vec4<f32>(s.rgb * a + d.rgb * (1.0 - a), clamp(a + d.a * (1.0 - a), 0.0, 1.0));
}
"#;

/// Program cache key for the scaled alpha-over pass
/// ([`SCALED_COMP_WGSL`]).
const SCALED_COMP_KEY: &str = "quercus/builtin/alpha-over-scaled";

/// WGSL fragment for the resolution-independent alpha-over: the source is
/// sampled at the destination pixel's normalized UV with a manual 4-tap
/// bilinear (edge-clamped `textureLoad`s — no sampler and no
/// `FLOAT32_FILTERABLE` requirement), so any source size/aspect maps onto
/// any target canvas in one GPU pass: pixel-exact when the sizes match,
/// scaled when they differ, never skipped. This keeps the timeline's
/// existing fill/stretch behavior (the CPU path pre-scales each clip with
/// swscale; this pass introduces no letterbox/pillarbox). `gain` is the
/// montage clip's playback gain (`1.0` for the graph compositor).
///
/// Bindings follow [`GpuContext::compile_shader_pass`]'s assignment:
/// binding 0 is the uniform, then the destination (1) and source (3)
/// texture pairs.
const SCALED_COMP_WGSL: &str = r#"
struct Gain {
    gain: vec4<f32>,
};
@group(0) @binding(0) var<uniform> g: Gain;
@group(0) @binding(1) var dst_tex: texture_2d<f32>;
@group(0) @binding(3) var src_tex: texture_2d<f32>;

// Bilinear tap at normalized `uv`, clamped to the texture's edge texels.
// Equal source/destination sizes land exactly on texel centers (f == 0),
// so the pass is a pixel-identity then.
fn sample_bilinear(tex: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    let dims = textureDimensions(tex);
    let size = vec2<f32>(f32(dims.x), f32(dims.y));
    let p = uv * size - vec2<f32>(0.5, 0.5);
    let base = floor(p);
    let f = p - base;
    let last = vec2<i32>(i32(dims.x), i32(dims.y)) - vec2<i32>(1, 1);
    let c = vec2<i32>(base);
    let c00 = clamp(c, vec2<i32>(0, 0), last);
    let c10 = clamp(c + vec2<i32>(1, 0), vec2<i32>(0, 0), last);
    let c01 = clamp(c + vec2<i32>(0, 1), vec2<i32>(0, 0), last);
    let c11 = clamp(c + vec2<i32>(1, 1), vec2<i32>(0, 0), last);
    let top = mix(textureLoad(tex, c00, 0), textureLoad(tex, c10, 0), f.x);
    let bottom = mix(textureLoad(tex, c01, 0), textureLoad(tex, c11, 0), f.x);
    return mix(top, bottom, f.y);
}

@fragment
fn main(@builtin(position) frag: vec4<f32>, @location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let dims = textureDimensions(dst_tex);
    let coord = clamp(
        vec2<u32>(u32(i32(frag.x)), u32(i32(frag.y))),
        vec2<u32>(0u, 0u),
        dims - vec2<u32>(1u, 1u),
    );
    let d = textureLoad(dst_tex, coord, 0);
    let s = sample_bilinear(src_tex, uv) * g.gain.x;
    let a = clamp(s.a, 0.0, 1.0);
    // RGB keeps the working-space values unclamped (HDR/WCG can exceed
    // 1.0); only the alpha of the result is clamped to its valid range.
    return vec4<f32>(s.rgb * a + d.rgb * (1.0 - a), clamp(a + d.a * (1.0 - a), 0.0, 1.0));
}
"#;

/// Program cache key for the GPU Opacity pass ([`OPACITY_GPU_WGSL`]).
const OPACITY_GPU_KEY: &str = "quercus/builtin/opacity-gpu";

/// WGSL for the GPU Opacity pass (C++ `:/shaders/opacity.frag` parity):
/// every channel, alpha included, is multiplied by `opacity_in`. Running
/// it on the GPU keeps the montage effect stack on the GPU end to end —
/// the built-in Opacity never forces a readback.
const OPACITY_GPU_WGSL: &str = r#"
struct Params {
    factor: vec4<f32>,
};
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var tex_in: texture_2d<f32>;

@fragment
fn main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = textureDimensions(tex_in);
    let coord = clamp(
        vec2<u32>(u32(i32(frag.x)), u32(i32(frag.y))),
        vec2<u32>(0u, 0u),
        dims - vec2<u32>(1u, 1u),
    );
    return textureLoad(tex_in, coord, 0) * p.factor.x;
}
"#;

/// Little-endian bytes of a one-lane `vec4<f32>` uniform (the opacity
/// factor or a clip gain; the other lanes are std140 padding).
fn scale_uniform_bytes(value: f32) -> [u8; 16] {
	let mut bytes = [0u8; 16];
	bytes[..4].copy_from_slice(&value.to_le_bytes());
	bytes
}

/// GPU composite of `frames` into one `(w, h)` texture: bottom (last) to
/// top (first), alpha-over into a ping-pong accumulator pair.
///
/// Frames at a resolution other than the composite target are scaled by
/// the normalized-UV bilinear pass instead of being skipped (`(w, h)` here
/// is the preview raster; a native 4K/portrait/any-aspect frame must still
/// fill it). This preserves the existing fill/stretch semantics — no
/// letterbox is introduced.
///
/// GPU→GPU (M2): textures already on the context are used by token; CPU
/// frames are uploaded into scratch textures (counted, and only when the
/// caller has a CPU frame in the stack). Nothing is read back — the
/// result stays on the GPU.
fn composite_tracks_gpu(
	ctx: &std::sync::Arc<quercus_core::backend::GpuContext>,
	frames: &[Texture],
	size: (i32, i32),
) -> Result<Texture> {
	let (w, h) = size;
	if w <= 0 || h <= 0 {
		return Err(Error::Invalid);
	}
	let program =
		ctx.compile_shader_pass("quercus/builtin/alpha-over", COMP_WGSL, 2, false, false)?;
	// Compiled lazily: only a stack that contains a size-mismatched frame
	// needs the scaled variant (the common graph/timeline case decodes at
	// the target size).
	let mut scaled_program: Option<Arc<quercus_core::backend::ShaderProgram>> = None;
	let acc = ctx.create_texture(w, h)?;
	if let Err(err) = ctx.clear_texture(acc) {
		ctx.destroy_texture(acc);
		return Err(err);
	}
	let mut scratch: Vec<u64> = Vec::new();
	let mut current = acc;
	let mut owned_current = true;
	let result = (|| -> Result<u64> {
		for frame in frames.iter().rev() {
			let (frame_w, frame_h) = frame.size();
			// A dummy/zero-sized frame has nothing to composite (and no
			// texture to create); a real frame at any resolution is scaled
			// by the pass below instead of being skipped.
			if frame.is_dummy() || frame_w <= 0 || frame_h <= 0 {
				continue;
			}
			// Prefer the texture's own context when it is this one; a GPU
			// texture from another context can only be read back. Uploads
			// use the frame's own size so the shader can do the sizing.
			let src_token = match frame {
				Texture::Gpu { token, .. } if ctx.has_texture(*token) => *token,
				Texture::Gpu { .. } => {
					let cpu = frame.to_frame()?;
					let t = ctx.create_texture(cpu.width, cpu.height)?;
					if let Err(err) = ctx.upload(t, &cpu) {
						ctx.destroy_texture(t);
						return Err(err);
					}
					scratch.push(t);
					t
				}
				Texture::Cpu(f) => {
					let t = ctx.create_texture(f.width, f.height)?;
					if let Err(err) = ctx.upload(t, f) {
						ctx.destroy_texture(t);
						return Err(err);
					}
					scratch.push(t);
					t
				}
				Texture::Planar(_) => {
					return Err(Error::Failed(
						"unresolved planar texture in composite".into(),
					))
				}
			};
			let out = ctx.create_texture(w, h)?;
			let pass = if (frame_w, frame_h) == (w, h) {
				ctx.run_shader_pass(&program, &[], &[current, src_token], out)
			} else {
				if scaled_program.is_none() {
					scaled_program = Some(ctx.compile_shader_pass(
						SCALED_COMP_KEY,
						SCALED_COMP_WGSL,
						2,
						true,
						false,
					)?);
				}
				let scaled = scaled_program.as_ref().expect("just compiled");
				ctx.run_shader_pass(
					scaled,
					&scale_uniform_bytes(1.0),
					&[current, src_token],
					out,
				)
			};
			if let Err(err) = pass {
				ctx.destroy_texture(out);
				return Err(Error::Failed(format!("alpha-over pass failed: {err:?}")));
			}
			if owned_current {
				ctx.destroy_texture(current);
			}
			current = out;
			owned_current = true;
		}
		Ok(current)
	})();
	for t in scratch {
		ctx.destroy_texture(t);
	}
	match result {
		Ok(token) => Ok(Texture::gpu(ctx.clone(), token, w, h, PixelFormat::F32)),
		Err(err) => {
			if owned_current {
				ctx.destroy_texture(current);
			}
			Err(err)
		}
	}
}

/// GPU failures are remembered: a device whose wgpu pipeline fails
/// validation (e.g. an adapter that advertises ComputePipeline yet lacks
/// the required features) fails EVERY frame otherwise — each attempt
/// recompiles the shader, surfaces a validation error and stalls the
/// playback tick (the "picture barely updates on NVIDIA" report). After
/// one failure the composite stays on the CPU path for the process.
static GPU_COMPOSITE_FAILED: std::sync::atomic::AtomicBool =
	std::sync::atomic::AtomicBool::new(false);

/// GPU montage setup failures (a device that cannot create the montage
/// pass pipeline) are remembered like [`GPU_COMPOSITE_FAILED`]: otherwise
/// every playback tick would recompile the shader, surface a validation
/// error and stall. Once latched, `render_montage_frame` stays on the CPU
/// compositor for the process.
static GPU_MONTAGE_FAILED: std::sync::atomic::AtomicBool =
	std::sync::atomic::AtomicBool::new(false);

/// Composite of `frames` into one `size` texture. The GPU path is
/// preferred whenever a context is available (M2: the graph stays on the
/// GPU end to end); the CPU path is the no-adapter fallback and the
/// explicit (counted) readback for GPU frames that must go through a CPU
/// consumer.
///
/// Compositing always runs — even a single frame passes through the
/// alpha-over over a transparent accumulator, which is the graph's
/// premultiply step (an adjustment sweep's 0.5-opacity result must become
/// 0.25 after its final composite). Frames arrive topmost first; both
/// paths composite the stack bottom-up (see [`composite_tracks_gpu`]).
fn composite_tracks(frames: Vec<Texture>, size: (i32, i32)) -> Texture {
	let (w, h) = size;
	if w <= 0 || h <= 0 {
		return Texture::dummy();
	}
	if !GPU_COMPOSITE_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
		if let Some(ctx) = quercus_core::backend::GpuContext::shared() {
			match composite_tracks_gpu(&ctx, &frames, (w, h)) {
				Ok(texture) => return texture,
				Err(Error::State) => {
					// CPU layer: use the CPU compositor for this frame only.
				}
				Err(err) => {
					eprintln!("GPU track composite failed, using CPU (and staying there): {err:#}");
					GPU_COMPOSITE_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
				}
			}
		}
	}
	// CPU fallback: read every GPU frame back (the explicit boundary) and
	// composite the CPU stack bottom-up.
	composite_tracks_cpu(&frames, (w, h))
}

/// The CPU half of [`composite_tracks`]: frames arrive topmost first
/// (the render walk inserts each track's frame at the front), so the
/// stack is composited from the bottom (last) up — exactly like
/// [`composite_tracks_gpu`]. This is the path every machine without a
/// working adapter runs, so the layer order must match the GPU pass.
fn composite_tracks_cpu(frames: &[Texture], size: (i32, i32)) -> Texture {
	let (w, h) = size;
	if w <= 0 || h <= 0 {
		return Texture::dummy();
	}
	let Ok(mut acc) = generate_frame(Rational::new(0, 1), (w, h), PixelFormat::F32) else {
		return Texture::dummy();
	};
	let acc_stride = acc.linesize_bytes() as i32;
	for texture in frames.iter().rev() {
		let Ok(frame) = texture.to_frame() else {
			continue;
		};
		if frame.width != w || frame.height != h {
			continue;
		}
		composite_over(
			&mut acc.data,
			acc_stride,
			w,
			h,
			&frame.data,
			frame.linesize_bytes() as i32,
			1.0,
		);
	}
	Texture::wrap_frame(acc)
}

/// One video track's contribution to [`render_graph_frame`], resolved
/// before any evaluation so the project lock is only borrowed immutably
/// (the adjustment sweep needs `&mut` on the very same graph).
enum TrackRenderStep {
	/// The track's enabled clips covering the frame time.
	Clips(Vec<quercus_node::id::NodeId>),
	/// The track's enabled transition block covering the frame time: the
	/// two blocks it joins are evaluated at `time` and blended with the
	/// transition's shader. `progress` is the block's position across its
	/// own span, in `0..=1` (`0.0` shows `out_block`, `1.0` shows
	/// `in_block`); `shader` is the style id the block's `type_in` combo
	/// selects.
	Transition {
		block: quercus_node::id::NodeId,
		/// The outgoing (previous) block — `None` for a head transition
		/// (no previous clip; fades in from black).
		out_block: Option<quercus_node::id::NodeId>,
		/// The incoming (next) block — `None` for a tail transition (no
		/// next clip; fades out to black).
		in_block: Option<quercus_node::id::NodeId>,
		progress: f64,
		shader: &'static str,
	},
	/// The track's enabled adjustment block covering the frame time: run
	/// its effect chain over every frame collected below and let the
	/// result replace them (C++ adjustment layers affect everything
	/// underneath). `progress` is the block's position across its own
	/// span, in `0..=1`.
	Adjustment {
		block: quercus_node::id::NodeId,
		progress: f64,
	},
}

/// Where `time` sits inside `in_..out`, in `0..=1` (the adjustment
/// layer's `progress_in`). A degenerate span reports 0.
fn layer_progress(in_: Rational, out: Rational, time: Rational) -> f64 {
	let (in_, out, time) = (in_.to_f64(), out.to_f64(), time.to_f64());
	if out > in_ {
		((time - in_) / (out - in_)).clamp(0.0, 1.0)
	} else {
		0.0
	}
}

/// Evaluate one block at `time` and return its texture (GPU when the
/// graph produced one). Every non-texture outcome — the evaluator
/// produced no texture channel or the handle is null — is `Ok(None)` so a
/// caller can fall back instead of failing the whole frame.
fn evaluate_block_frame(
	graph: &quercus_node::graph::Graph,
	traverser: &mut quercus_node::traverser::Traverser,
	hooks: &mut RenderEvalHooks,
	block: quercus_node::id::NodeId,
	time: Rational,
) -> Result<Option<Texture>> {
	let request = quercus_node::traverser::EvalRequest::new(block, time);
	let table = traverser
		.evaluate(graph, &request, hooks)
		.map_err(|e| Error::Failed(format!("graph evaluation of block {block:?} failed: {e:?}")))?;
	let Some(NodeValue::Texture(handle)) = table.get(quercus_node::value::ValueType::Texture)
	else {
		return Ok(None);
	};
	if handle.ctx.is_null() {
		return Ok(None);
	}
	let Some(texture) = (unsafe { quercus_node::handle::get_checked::<Texture>(handle) }).cloned()
	else {
		return Ok(None);
	};
	Ok(Some(texture))
}

/// Blend the two sides of a transition block at `time` with the block's
/// style shader: evaluate both sides, box a [`ShaderJobPayload`] carrying
/// the pair and the progress factor, and run it through the render
/// seam's shader-job path (the transition node behavior supplies the
/// fragment source for the shader id). `None` when a side produced no
/// texture or the job could not run (no GPU context, compile failure) —
/// the caller then falls back to the side that natively covers `time`.
fn blend_transition(
	graph: &quercus_node::graph::Graph,
	traverser: &mut quercus_node::traverser::Traverser,
	hooks: &mut RenderEvalHooks,
	out_block: Option<quercus_node::id::NodeId>,
	in_block: Option<quercus_node::id::NodeId>,
	time: Rational,
	progress: f64,
	shader: &str,
) -> Result<Option<Texture>> {
	use quercus_node::nodes::transitions;
	use quercus_node::value::NodeValueRow;

	let from = match out_block {
		Some(block) => evaluate_block_frame(graph, traverser, hooks, block, time)?,
		None => None,
	};
	let to = match in_block {
		Some(block) => evaluate_block_frame(graph, traverser, hooks, block, time)?,
		None => None,
	};
	if from.is_none() && to.is_none() {
		return Ok(None);
	}

	// A single-sided transition blends against transparent black (a head
	// fade-in from nothing, a tail fade-out to nothing) — the same
	// shaders, with one side generated empty. GPU-resident sides blend
	// against a GPU-cleared texture; the CPU fallback generates a frame.
	let size = from
		.as_ref()
		.or(to.as_ref())
		.map(|f| f.size())
		.or(hooks.frame_size)
		.unwrap_or((1, 1));
	let black = |size: (i32, i32)| -> Option<Texture> {
		if let Some(ctx) = quercus_core::backend::GpuContext::shared() {
			let token = ctx.create_texture(size.0, size.1).ok()?;
			ctx.clear_texture(token).ok()?;
			return Some(Texture::gpu(ctx, token, size.0, size.1, PixelFormat::F32));
		}
		generate_frame(time, size, PixelFormat::F32)
			.ok()
			.map(Texture::wrap_frame)
	};
	let from = from.or_else(|| black(size));
	let to = to.or_else(|| black(size));

	// Both sides present: blend them. GPU sides stay GPU (the shader-job
	// path consumes their tokens directly); CPU sides upload only inside
	// the shader job. The originals stay around for the fallback below.
	let blended = match (from.as_ref(), to.as_ref()) {
		(Some(from), Some(to)) => {
			let payload = ShaderJobPayload {
				node_id: quercus_node::id::NodeId::INVALID,
				time,
				iterations: 1,
				type_id: "org.olivevideoeditor.Olive.transition".to_string(),
				shader_id: shader.to_string(),
				effect_input: String::new(),
				params: NodeValueRow::from([
					(
						transitions::TEXTURE_INPUT.to_string(),
						texture_value(from.clone()),
					),
					(
						transitions::BLEND_INPUT.to_string(),
						texture_value(to.clone()),
					),
					(
						transitions::PROGRESS_INPUT.to_string(),
						NodeValue::Float(progress),
					),
				]),
				iterative_input: String::new(),
			};
			hooks.process_shader_job(&payload)
		}
		_ => None,
	};

	// Fallback: without a blend (a missing side, or a shader job that
	// could not run) show the side that natively covers `time` — the
	// outgoing block before the cut, the incoming one at or after it.
	Ok(blended.or(if progress < 0.5 { from } else { to }))
}

/// Walk an adjustment block's effect chain to its head: the first node
/// whose effect input has no upstream — the node a sweep must feed the
/// composited lower layers into. Returns that node and the input id to
/// connect on it. `None` when there is no chain (a bare adjustment layer
/// contributes nothing and is skipped entirely) or when the walk cannot
/// reach a head (unconnected effect input, or a cycle).
fn adjustment_chain_head(
	graph: &quercus_node::graph::Graph,
	block: quercus_node::id::NodeId,
) -> Option<(quercus_node::id::NodeId, String)> {
	let mut visited = std::collections::HashSet::new();
	let mut node = block;
	loop {
		if !visited.insert(node) {
			return None;
		}
		let entry = graph.get(node)?;
		let input = entry.core.effect_input.clone();
		if input.is_empty() || entry.core.get_input(&input).is_none() {
			return None;
		}
		match graph.connected_output(node, &input, -1) {
			Some(upstream) => node = upstream,
			// The block's own effect input is open: nothing to run.
			None if node == block => return None,
			None => return Some((node, input)),
		}
	}
}

/// Run the effect chain of the adjustment `block` over `below` (the
/// frames of every track underneath it, topmost first): composite them,
/// stand up a temporary texture source holding the result, wire it into
/// the chain head, evaluate the block, and tear the temporary node back
/// down before returning — the graph is part of the live project, so
/// anything inspecting it concurrently must never see a half-wired
/// sweep.
///
/// `Ok(None)` means the boundary changes nothing (no effect chain, or the
/// chain produced no texture); `Ok(Some(texture))` is the chain's output,
/// which replaces the lower layers. The sweep stays on the GPU when the
/// collected layers are GPU textures (M2).
fn flush_adjustment_layer(
	graph: &mut quercus_node::graph::Graph,
	traverser: &mut quercus_node::traverser::Traverser,
	hooks: &mut RenderEvalHooks,
	block: quercus_node::id::NodeId,
	below: &[Texture],
	size: (i32, i32),
	time: Rational,
	progress: f64,
) -> Result<Option<Texture>> {
	let Some((head, head_input)) = adjustment_chain_head(graph, block) else {
		return Ok(None);
	};
	let below = composite_tracks(below.to_vec(), size);
	let (core, behavior) = quercus_node::nodes::compositesource::create();
	let source = graph.add_node(core, behavior);
	if let Some(entry) = graph.get_mut(source) {
		entry.core.set_standard_value(
			quercus_node::nodes::compositesource::TEXTURE_INPUT,
			-1,
			texture_value(below),
		);
	}
	if let Err(err) = graph.connect(source, head, &head_input, -1) {
		let _ = graph.remove_node(source);
		return Err(Error::Failed(format!(
			"adjustment layer {block:?}: cannot feed {head_input} of {head:?}: {err:?}"
		)));
	}
	hooks.layer_progress = Some(progress);
	let evaluated = traverser.evaluate(
		graph,
		&quercus_node::traverser::EvalRequest::new(block, time),
		hooks,
	);
	hooks.layer_progress = None;
	let _ = graph.remove_node(source);
	let table = evaluated.map_err(|e| {
		Error::Failed(format!(
			"graph evaluation of adjustment layer {block:?} failed: {e:?}"
		))
	})?;
	let Some(NodeValue::Texture(handle)) = table.get(quercus_node::value::ValueType::Texture)
	else {
		return Ok(None);
	};
	if handle.ctx.is_null() {
		return Ok(None);
	}
	let Some(texture) = (unsafe { quercus_node::handle::get_checked::<Texture>(handle) }).cloned()
	else {
		return Ok(None);
	};
	Ok(Some(texture))
}

/// Render one frame of `viewer` (a sequence) at `time`: evaluate every
/// enabled clip overlapping `time` through the node graph (one traverser
/// pass per clip; the hooks' decoder cache is shared across clips) and
/// composite the resulting frames bottommost-first — in the video track
/// list the LAST track (the highest-numbered one) is the topmost stack
/// element (NLE stacking, matching the timeline UI).
///
/// A track whose enabled adjustment block covers `time` contributes an
/// adjustment sweep (see [`flush_adjustment_layer`]) instead of its clips:
/// everything collected below is composited and pushed through the block's
/// effect chain, and the result replaces the layer stack underneath, so a
/// single block affects every lower track at once.
///
/// A track whose enabled transition block covers `time` contributes one
/// blended frame (see [`blend_transition`]) instead of the clip that
/// covers `time` on its own: both blocks the transition joins are
/// evaluated and mixed by the style shader the block's `type_in` combo
/// selects. An adjustment block still wins over a transition on the same
/// track.
///
/// `size` is the decode target for every clip, so all frames composite
/// without per-frame scaling. Errors: `Invalid` for a non-F32 format or a
/// non-positive size, `NotFound` for a missing viewer or non-sequence.
pub fn render_graph_frame(
	project: &Mutex<quercus_node::project::Project>,
	viewer: quercus_node::id::NodeId,
	time: Rational,
	size: (i32, i32),
	format: PixelFormat,
) -> Result<Texture> {
	if format != PixelFormat::F32 {
		return Err(Error::Invalid);
	}
	let (w, h) = size;
	if w <= 0 || h <= 0 {
		return Err(Error::Invalid);
	}
	let mut project_guard = project.lock().unwrap();

	// Plan the video tracks bottommost-first, one step per track: the
	// sequence's track lists (video then audio — C++ `Sequence` keeps them
	// in the `k_track_input_format` array order), the video list's tracks
	// in stacking order (the list's last track is the top of the stack;
	// `composite_tracks` walks the frames in reverse and draws the first
	// frame last), then each track's blocks. The plan holds plain ids so
	// the evaluation pass below can borrow the graph mutably for an
	// adjustment sweep's temporary source node.
	let (steps, sequence_size) = {
		let graph = &project_guard.graph;
		let entry = graph.get(viewer).ok_or(Error::NotFound)?;
		let sequence = entry
			.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<quercus_node::sequence::SequenceBehavior>())
			.ok_or(Error::NotFound)?;

		let mut steps: Vec<TrackRenderStep> = Vec::new();
		for tl_id in &sequence.track_lists {
			let Some(tl) = graph.get(*tl_id) else {
				continue;
			};
			let Some(tl) = tl
				.behavior
				.as_any()
				.and_then(|a| a.downcast_ref::<quercus_node::track::TrackListBehavior>())
			else {
				continue;
			};
			if tl.kind != quercus_node::track::TrackType::Video {
				continue;
			}
			for track_id in &tl.tracks {
				let Some(track) = graph.get(*track_id) else {
					continue;
				};
				let Some(track) = track
					.behavior
					.as_any()
					.and_then(|a| a.downcast_ref::<quercus_node::track::TrackBehavior>())
				else {
					continue;
				};
				// An enabled adjustment block covering `time` takes over
				// the track: its sweep replaces that track's clip stack.
				let mut step = None;
				for block_id in &track.blocks {
					let Some(block) = graph.get(*block_id) else {
						continue;
					};
					let Some(adjustment) = block.behavior.as_any().and_then(|a| {
						a.downcast_ref::<quercus_node::block::AdjustmentBlockBehavior>()
					}) else {
						continue;
					};
					if adjustment.core.enabled
						&& time >= adjustment.core.in_()
						&& time < adjustment.core.out()
					{
						step = Some(TrackRenderStep::Adjustment {
							block: *block_id,
							progress: layer_progress(
								adjustment.core.in_(),
								adjustment.core.out(),
								time,
							),
						});
						break;
					}
				}
				// A transition block covering `time` blends the two blocks
				// it joins. The scan runs before the clip scan so the
				// blend replaces the plain clip read: in the first half of
				// the span the outgoing clip alone covers `time` (the cut
				// is its out-point), in the second the incoming one does,
				// so the clip path would otherwise hard-cut at the cut.
				// A block with an unconnected side falls through to the
				// clip path, which shows whichever clip covers `time`.
				if step.is_none() {
					for block_id in &track.blocks {
						let Some(block) = graph.get(*block_id) else {
							continue;
						};
						let Some(transition) = block.behavior.as_any().and_then(|a| {
							a.downcast_ref::<quercus_node::block::TransitionBlockBehavior>()
						}) else {
							continue;
						};
						if !(transition.core.enabled
							&& time >= transition.core.in_()
							&& time < transition.core.out())
						{
							continue;
						}
						// A transition needs at least one neighbor: a
						// junction block wires both, a head/tail
						// (single-sided) transition wires only its own
						// clip and fades from/to black.
						let (out_block, in_block) = (
							graph.connected_output(
								*block_id,
								quercus_node::block::transition_input::OUT_BLOCK,
								-1,
							),
							graph.connected_output(
								*block_id,
								quercus_node::block::transition_input::IN_BLOCK,
								-1,
							),
						);
						if out_block.is_none() && in_block.is_none() {
							continue;
						}
						let style = block
							.core
							.value_at_time(
								quercus_node::block::transition_input::TYPE_INPUT,
								-1,
								time,
							)
							.to_double() as i64;
						step = Some(TrackRenderStep::Transition {
							block: *block_id,
							out_block,
							in_block,
							progress: layer_progress(
								transition.core.in_(),
								transition.core.out(),
								time,
							),
							shader: quercus_node::nodes::transitions::shader_id_for(style),
						});
						break;
					}
				}
				if step.is_none() {
					let mut clips: Vec<quercus_node::id::NodeId> = Vec::new();
					for block_id in &track.blocks {
						let Some(block) = graph.get(*block_id) else {
							continue;
						};
						let Some(clip) = block.behavior.as_any().and_then(|a| {
							a.downcast_ref::<quercus_node::block::ClipBlockBehavior>()
						}) else {
							continue;
						};
						if clip.core.enabled && time >= clip.core.in_() && time < clip.core.out() {
							clips.push(*block_id);
						}
					}
					step = Some(TrackRenderStep::Clips(clips));
				}
				steps.push(step.unwrap());
			}
		}
		let sequence_size = sequence
			.video_params
			.first()
			.map(|p| (p.width.max(1), p.height.max(1)));
		(steps, sequence_size)
	};

	let mut traverser = quercus_node::traverser::Traverser::new();
	let mut hooks = RenderEvalHooks::new();
	hooks.frame_size = Some(size);
	// The generators' `resolution_in` anchor (C++ NodeGlobals square
	// resolution): the sequence's native size, so proxy-size playback and
	// full-res paused frames draw generated layers identically.
	hooks.sequence_size = sequence_size;
	let perf = std::env::var_os("QUERCUS_PERF").is_some();
	let mut perf_collect = perf.then(std::time::Instant::now);
	let mut perf_collect_ms = 0.0f64;
	// Topmost frame first (see the track walk above).
	let mut frames: Vec<Texture> = Vec::new();
	let mut perf_clip_hist: Vec<(&'static str, quercus_node::id::NodeId, f64)> = Vec::new();
	for step in steps {
		match step {
			TrackRenderStep::Clips(clips) => {
				for clip in clips {
					let clip_sw = perf.then(std::time::Instant::now);
					let request = quercus_node::traverser::EvalRequest::new(clip, time);
					let table = traverser
						.evaluate(&project_guard.graph, &request, &mut hooks)
						.map_err(|e| {
							Error::Failed(format!(
								"graph evaluation of clip {clip:?} failed: {e:?}"
							))
						})?;
					if perf {
						perf_clip_hist.push((
							"clip",
							clip,
							clip_sw
								.map(|t| t.elapsed().as_secs_f64() * 1000.0)
								.unwrap_or(0.0),
						));
					}
					if let Some(t0) = &perf_collect {
						perf_collect_ms += t0.elapsed().as_secs_f64() * 1000.0;
						perf_collect = Some(std::time::Instant::now());
					}
					let Some(NodeValue::Texture(handle)) =
						table.get(quercus_node::value::ValueType::Texture)
					else {
						continue;
					};
					if handle.ctx.is_null() {
						continue;
					}
					let Some(texture) =
						(unsafe { quercus_node::handle::get_checked::<Texture>(handle) }).cloned()
					else {
						continue;
					};
					frames.insert(0, texture);
				}
			}
			TrackRenderStep::Transition {
				block,
				out_block,
				in_block,
				progress,
				shader,
			} => {
				let transition_sw = perf.then(std::time::Instant::now);
				let blended = blend_transition(
					&project_guard.graph,
					&mut traverser,
					&mut hooks,
					out_block,
					in_block,
					time,
					progress,
					shader,
				)?;
				if perf {
					perf_clip_hist.push((
						"transition",
						block,
						transition_sw
							.map(|t| t.elapsed().as_secs_f64() * 1000.0)
							.unwrap_or(0.0),
					));
				}
				if let Some(t0) = &perf_collect {
					perf_collect_ms += t0.elapsed().as_secs_f64() * 1000.0;
					perf_collect = Some(std::time::Instant::now());
				}
				if let Some(frame) = blended {
					frames.insert(0, frame);
				}
			}
			TrackRenderStep::Adjustment { block, progress } => {
				let sweep_sw = perf.then(std::time::Instant::now);
				let swept = flush_adjustment_layer(
					&mut project_guard.graph,
					&mut traverser,
					&mut hooks,
					block,
					&frames,
					size,
					time,
					progress,
				)?;
				if perf {
					perf_clip_hist.push((
						"adjustment",
						block,
						sweep_sw
							.map(|t| t.elapsed().as_secs_f64() * 1000.0)
							.unwrap_or(0.0),
					));
				}
				if let Some(t0) = &perf_collect {
					perf_collect_ms += t0.elapsed().as_secs_f64() * 1000.0;
					perf_collect = Some(std::time::Instant::now());
				}
				if let Some(frame) = swept {
					frames = vec![frame];
				}
			}
		}
	}

	let composite_started = perf.then(std::time::Instant::now);
	let frame = composite_tracks(frames, size).with_timestamp(time);
	if perf {
		let composite_ms = composite_started
			.map(|t| t.elapsed().as_secs_f64() * 1000.0)
			.unwrap_or(0.0);
		for (kind, node, ms) in &perf_clip_hist {
			eprintln!("[perf]   {kind} {node:?} evaluate {ms:.1}ms");
		}
		eprintln!(
            "[perf] graph frame time {time:?} size {size:?}: evaluate {perf_collect_ms:.1}ms composite {composite_ms:.1}ms total {}ms",
            perf_collect_ms + composite_ms
        );
	}
	Ok(frame)
}

/// Render the audio montage over `params.range` (M12 P1): every clip
/// overlapping the range is decoded (interleaved f32 at the output rate
/// and layout) and mixed with its gain; uncovered parts stay silent. The
/// mixed output is clamped to [-1, 1].
/// Bilinear scale an F32-RGBA image (row-major with per-row strides).
///
/// `src` points at the first source row and `src_stride` is the source row
/// stride in bytes; `src_w`/`src_h` are the source dimensions in pixels.
/// `dst` receives the scaled image with `dst_stride` bytes per row and
/// `dst_w`/`dst_h` pixels. Sample positions use half-pixel centers and
/// clamp to the image border, so edge texels are replicated. RGB is
/// written unclamped (linear-light values above 1.0 are preserved); only
/// alpha is clamped to `[0.0, 1.0]`.
///
/// Any non-positive dimension returns without writing `dst`.
///
/// The caller must guarantee that `src` is readable for `src_h` rows of
/// `src_stride` bytes (at least `src_w * 16` bytes used per row) and that
/// `dst` holds at least `(dst_h - 1) * dst_stride + dst_w * 16` bytes; the
/// function performs no bounds checks against the real allocations.
fn scale_rgba_f32(
	src: *const u8,
	src_stride: i32,
	src_w: i32,
	src_h: i32,
	dst: &mut [u8],
	dst_stride: i32,
	dst_w: i32,
	dst_h: i32,
) {
	if src_w <= 0 || src_h <= 0 || dst_w <= 0 || dst_h <= 0 {
		return;
	}
	// 1:1 copy (no scaling): the caller already handled stride equality;
	// here we handle the general case with a fast path for integer 1:1.
	let src_addr = src as usize;
	let sample = |x: f64, y: f64| -> [f32; 4] {
		let x0 = x.floor() as i32;
		let y0 = y.floor() as i32;
		let fx = (x - x0 as f64) as f32;
		let fy = (y - y0 as f64) as f32;
		let x1 = (x0 + 1).clamp(0, src_w - 1);
		let y1 = (y0 + 1).clamp(0, src_h - 1);
		let x0 = x0.clamp(0, src_w - 1);
		let y0 = y0.clamp(0, src_h - 1);
		let px = |xx: i32, yy: i32| -> [f32; 4] {
			let off = (yy as usize) * (src_stride as usize) + (xx as usize) * 16;
			// SAFETY: coordinates are clamped to the source size.
			let src = src_addr as *const u8;
			let b = unsafe { std::slice::from_raw_parts(src.add(off), 16) };
			let f = |i: usize| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
			[f(0), f(1), f(2), f(3)]
		};
		let c00 = px(x0, y0);
		let c10 = px(x1, y0);
		let c01 = px(x0, y1);
		let c11 = px(x1, y1);
		let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
		let mut out = [0f32; 4];
		for i in 0..4 {
			let top = lerp(c00[i], c10[i], fx);
			let bottom = lerp(c01[i], c11[i], fx);
			out[i] = lerp(top, bottom, fy);
		}
		out
	};
	quercus_core::cpuparallel::for_each_row(
		dst,
		dst_stride as usize,
		dst_h as usize,
		dst_w as usize,
		|y, row| {
			let sy = (y as f64 + 0.5) * src_h as f64 / dst_h as f64 - 0.5;
			let sy = sy.max(0.0);
			for x in 0..dst_w {
				let sx = (x as f64 + 0.5) * src_w as f64 / dst_w as f64 - 0.5;
				let sx = sx.max(0.0);
				let px = sample(sx, sy);
				let off = (x as usize) * 16;
				// RGB is not clamped: bilinear lerp is a convex combination,
				// so values cannot overshoot the source range, and HDR/WCG
				// working-space pixels may legitimately exceed 1.0. Only the
				// alpha channel is clamped to its valid range.
				for i in 0..4 {
					let v = if i == 3 { px[i].clamp(0.0, 1.0) } else { px[i] };
					row[off + i * 4..off + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
				}
			}
		},
	);
}

/// Composite the montage at `time` for the ticket/eval path. The GPU
/// compositor ([`render_montage_frame_gpu`]) is preferred whenever an
/// adapter is available and the ticket asks for the canonical F32 frame —
/// the normal playback path, where decode, effects and compositing stay
/// on the GPU end to end. Every other case (no adapter, a non-F32 ticket,
/// a per-frame GPU failure) falls back to the unchanged CPU compositor
/// [`render_montage_frame_into`].
///
/// Public so the engine's preview/worker path can request the GPU-resident
/// montage texture instead of re-running the CPU compositor. The worker's
/// shared-memory wire format still reads the texture back at its boundary;
/// that is the protocol boundary, not a decode/scale readback.
pub fn render_montage_frame(
	time: Rational,
	params: &crate::ticket::VideoTicketParams,
	size: (i32, i32),
	format: PixelFormat,
) -> Result<Texture> {
	if format == PixelFormat::F32 && !GPU_MONTAGE_FAILED.load(std::sync::atomic::Ordering::Relaxed)
	{
		if montage_has_cpu_effects(params) {
			if unsupported_warned().insert("montage-cpu-effects".to_string()) {
				eprintln!(
					"montage has CPU-only effects; using the CPU compositor (readback boundary)"
				);
			}
		} else if let Some(ctx) = quercus_core::backend::GpuContext::shared() {
			match render_montage_frame_gpu(&ctx, time, params, size) {
				Ok(texture) => return Ok(texture),
				Err(err) => {
					// Log once per process; a decode failure repeats on the
					// CPU path below, which reports its own error.
					if unsupported_warned().insert("montage-gpu".to_string()) {
						eprintln!(
							"GPU montage composite failed, using the CPU compositor: {err:#}"
						);
					}
				}
			}
		}
	}
	let mut acc = generate_frame(time, size, format)?;
	let stride = acc.linesize_bytes();
	let acc_data = &mut acc.data;
	render_montage_frame_into(time, params, size, acc_data, stride as i32)?;
	Ok(Texture::wrap_frame(acc))
}

/// Whether `params` should be composited by the GPU montage path.
///
/// True only when the ticket asks for the canonical F32 frame, a shared
/// [`GpuContext`] exists, no montage effect is CPU-only (see
/// [`montage_has_cpu_effects`]) and no earlier frame latched
/// `GPU_MONTAGE_FAILED`. Cheap and side-effect free; the pipeline uses it
/// to decide whether speculative decode requests may import planar textures
/// ([`DecodedFrameKey`]'s import flag). [`render_montage_frame`] is the
/// consumer and re-checks the same conditions, falling back to the CPU
/// compositor.
pub(crate) fn montage_prefers_gpu(
	params: &crate::ticket::VideoTicketParams,
	format: PixelFormat,
) -> bool {
	format == PixelFormat::F32
		&& !GPU_MONTAGE_FAILED.load(std::sync::atomic::Ordering::Relaxed)
		&& !montage_has_cpu_effects(params)
		&& quercus_core::backend::GpuContext::shared().is_some()
}

/// 1×1 transparent F32 GPU base for the first montage layer.
///
/// The texture is borrowed from `ctx`'s placeholder pool
/// ([`GpuContext::placeholder_texture`]) rather than allocated and
/// cleared, so the first alpha-over pass can replace it with a full-size
/// accumulator without a per-frame `clear_texture` submit.
fn transparent_gpu_base(
	ctx: &std::sync::Arc<quercus_core::backend::GpuContext>,
) -> Result<Texture> {
	let token = ctx.placeholder_texture()?;
	let lease = quercus_core::texture::GpuLease::borrowed(ctx.clone(), token);
	Ok(Texture::Gpu {
		token,
		backend: ctx.kind(),
		width: 1,
		height: 1,
		format: PixelFormat::F32,
		ctx: ctx.clone(),
		lease,
		timestamp: Rational::default(),
	})
}

/// GPU montage compositor (the playback path's replacement for the CPU
/// loop in [`render_montage_frame_into`]).
///
/// Each covering clip is decoded through the M5 GPU-import decode
/// ([`render_footage_frame`], import enabled — never
/// [`render_footage_frame_staged`]); imported planar frames are resolved
/// to the preview size on the GPU by [`resolve_planar_footage`]. The clip
/// effect stack then runs (built-in Opacity on the GPU; CPU-only effects
/// are the explicit boundary) and the result is alpha-overed into a GPU
/// accumulator by [`composite_montage_gpu`]. Adjustment spans grade the
/// accumulator between clips in the same order as the CPU compositor, so
/// the layer semantics match.
///
/// The result is always a `Texture::Gpu` (F32). Nothing is downloaded on
/// the normal path: the only transfers are uploads of CPU-decoded frames
/// (software decode) and the explicit readback of a texture that belongs
/// to a different context.
///
/// `ctx` is the shared GPU context the returned texture belongs to;
/// `time` is the sequence time to composite; `params` carries the
/// bottom-up `montage` clip list and the adjustment spans; `size` is the
/// output size in pixels.
///
/// # Errors
///
/// `Error::Invalid` when `size` has a non-positive dimension, plus
/// propagated shader-compile, allocation, decode and composite errors. A
/// shader-compile failure also latches `GPU_MONTAGE_FAILED`, so
/// [`render_montage_frame`] keeps using the CPU compositor for the rest of
/// the process instead of retrying the pipeline every frame.
fn render_montage_frame_gpu(
	ctx: &std::sync::Arc<quercus_core::backend::GpuContext>,
	time: Rational,
	params: &crate::ticket::VideoTicketParams,
	size: (i32, i32),
) -> Result<Texture> {
	let (w, h) = size;
	if w <= 0 || h <= 0 {
		return Err(Error::Invalid);
	}
	let program = match ctx.compile_shader_pass(SCALED_COMP_KEY, SCALED_COMP_WGSL, 2, true, false) {
		Ok(program) => program,
		Err(err) => {
			// A pipeline this device cannot create fails every frame;
			// latch it so playback stays on the CPU compositor for good
			// instead of recompiling and stalling each tick.
			GPU_MONTAGE_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
			return Err(err);
		}
	};
	let acc = ctx.create_texture(w, h)?;
	let mut acc_owned = true;
	let mut initialized = false;
	let mut current = transparent_gpu_base(ctx)?;
	let mut spans = params.adjustments.iter().peekable();
	// Decode from the bottom clip first, composite topmost-last — the same
	// order as the CPU compositor (the montage list is bottom-up).
	for (index, clip) in params.montage.iter().enumerate() {
		// Every layer whose boundary is reached by the clips composited so
		// far grades the accumulated frame before this clip lands.
		while spans.peek().is_some_and(|span| span.track_index <= index) {
			let span = spans.next().expect("peeked");
			if time >= span.in_time && time < span.out_time {
				if !initialized {
					if let Err(err) = ctx.clear_texture(acc) {
						ctx.destroy_texture(acc);
						return Err(err);
					}
					current = Texture::gpu(ctx.clone(), acc, w, h, PixelFormat::F32);
					acc_owned = false;
					initialized = true;
				}
				current = apply_effect_list(current, &span.effects, time);
			}
		}
		if time < clip.in_time || time >= clip.out_time {
			continue;
		}
		let media_time = clip.media_in + (time - clip.in_time);
		// GPU import enabled: a hardware-planar frame resolves through the
		// scaled matrix pass straight to the preview size (no CPU swscale,
		// no native-size RGBA intermediate).
		let decoded = render_footage_frame(
			&clip.filename,
			clip.stream_index,
			media_time,
			(w, h),
			PixelFormat::F32,
		)?;
		// The clip's effect stack runs between decode and compositing
		// (C++ semantics: the clip texture passes through the chain
		// bottom-up, the chain top feeds the track composite).
		let effected = apply_clip_effects(decoded, clip, time);
		current = composite_montage_gpu(ctx, &program, &current, &effected, clip.gain, size)?;
		initialized = true;
	}
	// Layers above every clip (their boundary is the montage end): they
	// grade the final composite.
	for span in spans {
		if time >= span.in_time && time < span.out_time {
			if !initialized {
				if let Err(err) = ctx.clear_texture(acc) {
					ctx.destroy_texture(acc);
					return Err(err);
				}
				current = Texture::gpu(ctx.clone(), acc, w, h, PixelFormat::F32);
				acc_owned = false;
				initialized = true;
			}
			current = apply_effect_list(current, &span.effects, time);
		}
	}
	if !initialized {
		if let Err(err) = ctx.clear_texture(acc) {
			ctx.destroy_texture(acc);
			return Err(err);
		}
		current = Texture::gpu(ctx.clone(), acc, w, h, PixelFormat::F32);
		acc_owned = false;
	}
	if acc_owned {
		ctx.destroy_texture(acc);
	}
	Ok(current)
}

/// Alpha-over `src` onto `dst` on the GPU with `gain`, each at its own
/// resolution: the scaled pass samples `src` at the destination pixel's
/// normalized UV, so any source size/aspect fills any preview canvas
/// (existing fill/stretch semantics; no letterbox) and mismatched sizes
/// are never skipped. `dst`/`src` are only borrowed; the returned texture
/// owns the new accumulator.
fn composite_montage_gpu(
	ctx: &std::sync::Arc<quercus_core::backend::GpuContext>,
	program: &quercus_core::backend::ShaderProgram,
	dst: &Texture,
	src: &Texture,
	gain: f32,
	size: (i32, i32),
) -> Result<Texture> {
	let (w, h) = size;
	let mut scratch: Vec<u64> = Vec::new();
	let result = (|| -> Result<Texture> {
		let dst_token = gpu_composite_token(ctx, dst, &mut scratch)?;
		let src_token = gpu_composite_token(ctx, src, &mut scratch)?;
		let out = ctx.create_texture(w, h)?;
		if let Err(err) = ctx.run_shader_pass(
			program,
			&scale_uniform_bytes(gain),
			&[dst_token, src_token],
			out,
		) {
			ctx.destroy_texture(out);
			return Err(Error::Failed(format!(
				"montage alpha-over pass failed: {err:?}"
			)));
		}
		Ok(Texture::gpu(ctx.clone(), out, w, h, PixelFormat::F32))
	})();
	for token in scratch {
		ctx.destroy_texture(token);
	}
	result
}

/// The backend token of `texture` on `ctx`, for a GPU composite pass: a
/// GPU texture owned by this context is bound directly; a CPU frame is
/// uploaded at its own size (the scaled shader does the sizing); a GPU
/// texture from another context is read back and re-uploaded (the
/// explicit foreign-context boundary). Tokens created here are pushed to
/// `scratch` for the caller to destroy after the pass.
fn gpu_composite_token(
	ctx: &std::sync::Arc<quercus_core::backend::GpuContext>,
	texture: &Texture,
	scratch: &mut Vec<u64>,
) -> Result<u64> {
	match texture {
		Texture::Gpu { token, .. } if ctx.has_texture(*token) => Ok(*token),
		Texture::Gpu { .. } => {
			let frame = texture.to_frame()?;
			let token = ctx.create_texture(frame.width, frame.height)?;
			if let Err(err) = ctx.upload(token, &frame) {
				ctx.destroy_texture(token);
				return Err(err);
			}
			scratch.push(token);
			Ok(token)
		}
		Texture::Cpu(frame) => {
			let token = ctx.create_texture(frame.width, frame.height)?;
			if let Err(err) = ctx.upload(token, frame) {
				ctx.destroy_texture(token);
				return Err(err);
			}
			scratch.push(token);
			Ok(token)
		}
		Texture::Planar(_) => Err(Error::Failed(
			"unresolved planar texture in the GPU montage composite".into(),
		)),
	}
}

/// Composite the montage at `time` directly into `dst` (F32 RGBA rows of
/// `dst_stride` bytes) — the M15 worker seam: the render worker passes a
/// shared-memory slot slice as `dst`, so the composited frame lands in
/// the slot with no staging copy. `dst` is zeroed first (transparent
/// black base).
///
/// This is the CPU compositor and stays the worker/no-adapter fallback;
/// [`render_montage_frame`] prefers the GPU compositor
/// ([`render_montage_frame_gpu`]) for F32 tickets and only calls this
/// entry point when no adapter is available (or on a per-frame GPU
/// failure). Its signature/behavior are unchanged.
///
/// Adjustment layers (`params.adjustments`, M14 W3) sit between the
/// clips: a span's effect stack runs over the frame accumulated so far
/// once `span.track_index` clips below it have been composited — the
/// montage twin of the graph path's `flush_adjustment_layer`, which
/// grades everything under the layer and leaves the tracks above
/// compositing over that result. Spans arrive bottom-up (non-decreasing
/// boundary); the loop consumes them in list order.
pub fn render_montage_frame_into(
	time: Rational,
	params: &crate::ticket::VideoTicketParams,
	size: (i32, i32),
	dst: &mut [u8],
	dst_stride: i32,
) -> Result<()> {
	let (w, h) = size;
	let need = (h as usize).saturating_mul(dst_stride as usize);
	if w <= 0 || h <= 0 || dst.len() < need {
		return Err(Error::Invalid);
	}
	// Transparent-black base.
	dst[..need].fill(0);
	let mut spans = params.adjustments.iter().peekable();
	// Decode from the bottom clip first, composite topmost-last.
	for (index, clip) in params.montage.iter().enumerate() {
		// Every layer whose boundary is reached by the clips composited
		// so far grades the accumulated frame before this clip lands.
		while spans.peek().is_some_and(|span| span.track_index <= index) {
			let span = spans.next().expect("peeked");
			apply_adjustment_span(dst, dst_stride, w, h, span, time);
		}
		if time < clip.in_time || time >= clip.out_time {
			continue;
		}
		let media_time = clip.media_in + (time - clip.in_time);
		// CPU compositor: stage on purpose (never an imported GPU frame).
		let decoded = render_footage_frame_staged(
			&clip.filename,
			clip.stream_index,
			media_time,
			(w, h),
			PixelFormat::F32,
		)?;
		// The clip's effect stack runs between decode and compositing
		// (C++ semantics: the clip texture passes through the chain
		// bottom-up, the chain top feeds the track composite).
		let effected = apply_clip_effects(decoded, clip, time);
		// The compositor is CPU-side. The decode is staged above, but a
		// clip effect may still hand back a GPU texture: read it back
		// (an explicit CPU boundary) instead of silently dropping the
		// clip (M5 audit: that skip produced all-black sequences).
		let downloaded: Frame;
		let (src_data, src_stride) = match &effected {
			Texture::Cpu(src) => (&src.data, src.linesize_bytes() as i32),
			Texture::Gpu { .. } => {
				downloaded = effected
					.to_frame()
					.map_err(|e| Error::Failed(format!("montage GPU readback failed: {e:?}")))?;
				(&downloaded.data, downloaded.linesize_bytes() as i32)
			}
			Texture::Planar(_) => {
				return Err(Error::Failed(
					"unresolved planar texture reached the montage compositor".into(),
				))
			}
		};
		composite_over(dst, dst_stride, w, h, src_data, src_stride, clip.gain);
	}
	// Layers above every clip (their boundary is the montage end): they
	// grade the final composite.
	for span in spans {
		apply_adjustment_span(dst, dst_stride, w, h, span, time);
	}
	Ok(())
}

/// Apply an adjustment layer's effect stack to the frame accumulated so
/// far — the montage twin of the graph path's `flush_adjustment_layer`.
/// The stack runs source-first over `dst` before the clips above the
/// layer are composited, so the layer grades exactly the picture
/// underneath it (C++: the adjustment node's `texture_input` chain
/// output passes through its effect chain, and that output is what the
/// tracks above composite over; a bare adjustment layer passes the
/// frame through unchanged).
///
/// Known limitation (M14 W3 — the same one the clip stacks carry): the
/// montage evaluator only covers the built-in Opacity effect and OFX
/// plugins; other built-ins (color management, transforms, generated
/// textures…) log a warning once and pass the frame through. Graph mode
/// (`render_graph_frame`, M14 W2) evaluates the full built-in set, so a
/// worker holding a matching project snapshot stays the accurate
/// preview and this path is the montage fallback.
fn apply_adjustment_span(
	dst: &mut [u8],
	dst_stride: i32,
	w: i32,
	h: i32,
	span: &crate::ticket::AdjustmentSpan,
	time: Rational,
) {
	// Spans are baked for the ticket's own time; one that does not cover
	// it (a stale or foreign list) is inert, matching the graph path's
	// range test.
	if time < span.in_time || time >= span.out_time {
		return;
	}
	// Fast path: an all-Opacity stack (the common case) scales the
	// accumulated frame in place, no staging copy — opacity is a pure
	// per-channel multiply, alpha included (C++ `:/shaders/opacity.frag`).
	let mut factors = Vec::new();
	let mut opacity_only = true;
	for effect in span.effects.iter().filter(|e| e.enabled) {
		match opacity_factor(effect) {
			Some(factor) => factors.push(factor),
			// Unity is a pass-through; any other type needs the staged
			// path below.
			None if effect.type_id == OPACITY_EFFECT_TYPE_ID => {}
			None => {
				opacity_only = false;
				break;
			}
		}
	}
	if opacity_only {
		for factor in factors {
			scale_channels_in_place(dst, dst_stride as usize, w, h, factor);
		}
		return;
	}
	// General path: the effect evaluator takes and returns an owned
	// texture, so stage the accumulated frame into one and copy the
	// result back.
	let Ok(mut frame) = generate_frame(time, (w, h), PixelFormat::F32) else {
		return;
	};
	let frame_stride = frame.linesize_bytes();
	if !copy_rows(
		&mut frame.data,
		frame_stride,
		dst,
		dst_stride as usize,
		w,
		h,
	) {
		return;
	}
	let tex = apply_effect_list(Texture::wrap_frame(frame), &span.effects, time);
	if let Texture::Cpu(out) = &tex {
		copy_rows(
			dst,
			dst_stride as usize,
			&out.data,
			out.linesize_bytes(),
			w,
			h,
		);
	}
}

/// Copy `h` rows of `w * 16` bytes between two F32 RGBA buffers with
/// different strides (the montage pipeline writes packed frames, slots
/// carry their own stride). False — nothing copied — when either buffer
/// is too small for the requested geometry.
fn copy_rows(
	dst: &mut [u8],
	dst_stride: usize,
	src: &[u8],
	src_stride: usize,
	w: i32,
	h: i32,
) -> bool {
	if w <= 0 || h <= 0 {
		return false;
	}
	let row_bytes = (w as usize) * 16;
	let rows = h as usize;
	let span = |stride: usize| (rows - 1) * stride + row_bytes;
	if src.len() < span(src_stride) || dst.len() < span(dst_stride) {
		return false;
	}
	quercus_core::cpuparallel::for_each_row(dst, dst_stride, rows, w as usize, |y, row| {
		let s = y * src_stride;
		row[..row_bytes].copy_from_slice(&src[s..s + row_bytes]);
	});
	true
}

/// `src` over `dst` (premultiplied-ish alpha compositing; F32 RGBA).
/// `gain` scales the source RGB (audio-style volume applied to video
/// transparency is ignored here; gain scales color). Exposed for the M15
/// render worker, which composites montage frames directly into
/// shared-memory slots.
pub fn composite_over(
	dst: &mut [u8],
	dst_stride: i32,
	w: i32,
	h: i32,
	src: &[u8],
	src_stride: i32,
	gain: f32,
) {
	let read_src = |x: i32, y: usize| -> [f32; 4] {
		let off = (y as usize) * (src_stride as usize) + (x as usize) * 16;
		let mut out = [0f32; 4];
		for i in 0..4 {
			out[i] = f32::from_le_bytes(src[off + i * 4..off + i * 4 + 4].try_into().unwrap());
		}
		out
	};
	quercus_core::cpuparallel::for_each_row(
		dst,
		dst_stride as usize,
		h as usize,
		w as usize,
		|y, row| {
			for x in 0..w {
				let s = read_src(x, y);
				let off = (x as usize) * 16;
				let mut d = [0f32; 4];
				for i in 0..4 {
					d[i] =
						f32::from_le_bytes(row[off + i * 4..off + i * 4 + 4].try_into().unwrap());
				}
				let a = (s[3] * gain).clamp(0.0, 1.0);
				let out = [
					(s[0] * gain) * a + d[0] * (1.0 - a),
					(s[1] * gain) * a + d[1] * (1.0 - a),
					(s[2] * gain) * a + d[2] * (1.0 - a),
					a + d[3] * (1.0 - a),
				];
				// RGB keeps the working-space values unclamped (HDR/WCG can
				// exceed 1.0); only alpha is clamped to its valid range.
				for i in 0..4 {
					let v = if i == 3 {
						out[i].clamp(0.0, 1.0)
					} else {
						out[i]
					};
					row[off + i * 4..off + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
				}
			}
		},
	);
}

// ---------------------------------------------------------------------------
// Montage clip effect stacks
// ---------------------------------------------------------------------------

/// The built-in Opacity effect's type id (quercusnode `OpacityEffect`) — the
/// one built-in video effect with a CPU evaluator on the montage path.
const OPACITY_EFFECT_TYPE_ID: &str = "org.olivevideoeditor.Olive.opacity";

/// The Opacity effect's value input id (quercusnode `opacity_in`).
const OPACITY_VALUE_INPUT: &str = "opacity_in";

/// The effect type ids the montage path already warned about (one log
/// line per type per process instead of one per frame).
fn unsupported_warned() -> std::sync::MutexGuard<'static, std::collections::HashSet<String>> {
	static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
		std::sync::OnceLock::new();
	WARNED
		.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
		.lock()
		.unwrap_or_else(|e| e.into_inner())
}

/// Build (and cache) the 3D LUT for an OCIO color processor (M2): the
/// CPU reference (`convert_f32_rgba`) fills the grid, and the per-pixel
/// transform then runs on the GPU via `GpuContext::apply_color_lut` —
/// color management is never skipped on a GPU texture. Cached by the
/// processor's OCIO cache id.
fn color_transform_lut(
	processor: &quercus_core::color::ColorProcessor,
) -> Option<std::sync::Arc<quercus_core::lut::Lut3d>> {
	type Cache = std::sync::Mutex<
		std::collections::HashMap<String, std::sync::Arc<quercus_core::lut::Lut3d>>,
	>;
	static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
	let key = processor.cache_id();
	let mut cache = CACHE
		.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	if let Some(lut) = cache.get(&key) {
		return Some(lut.clone());
	}
	let lut = build_color_transform_lut(processor)?;
	if cache.len() >= 16 {
		cache.clear();
	}
	cache.insert(key, lut.clone());
	Some(lut)
}

/// Bake a processor into a 3D LUT over the display domain (scene-linear
/// working values; the same range the presentation LUT covers).
fn build_color_transform_lut(
	processor: &quercus_core::color::ColorProcessor,
) -> Option<std::sync::Arc<quercus_core::lut::Lut3d>> {
	use quercus_core::lut::Lut3d;
	let edge = Lut3d::DISPLAY_EDGE;
	let (lo, hi) = (Lut3d::DISPLAY_LO, Lut3d::DISPLAY_HI);
	let n = (edge as usize).pow(3);
	let mut samples = vec![0.0f32; n * 4];
	let step = |i: usize, axis: usize| -> f32 {
		let t = i as f32 / (edge - 1) as f32;
		lo[axis] + (hi[axis] - lo[axis]) * t
	};
	for b in 0..edge as usize {
		for g in 0..edge as usize {
			for r in 0..edge as usize {
				let idx = ((b * edge as usize + g) * edge as usize + r) * 4;
				samples[idx] = step(r, 0);
				samples[idx + 1] = step(g, 1);
				samples[idx + 2] = step(b, 2);
				samples[idx + 3] = 1.0;
			}
		}
	}
	processor.convert_f32_rgba(&mut samples, n as i64).ok()?;
	let mut data = Vec::with_capacity(n * 3);
	for px in samples.chunks_exact(4) {
		data.extend_from_slice(&px[..3]);
	}
	Some(std::sync::Arc::new(Lut3d { edge, lo, hi, data }))
}

/// Log once per process that an effect passes through unchanged, keyed by
/// `type_id`, and drop further reports for the same type.
///
/// `type_id` is the effect/node type id used as the de-duplication key (it
/// need not be a currently registered type); `reason` is the human-readable
/// explanation printed after the type id on the first occurrence. Writes a
/// single line to standard error; never panics and returns no status.
fn warn_unsupported_once(type_id: &str, reason: &str) {
	if unsupported_warned().insert(type_id.to_string()) {
		eprintln!("montage effect \"{type_id}\" passes through unchanged: {reason}");
	}
}

/// Warn once (per process) when a decoded frame carries no colorimetry
/// metadata and falls back to the generic sRGB assumptions.
fn warn_missing_colorimetry_once() {
	static WARNED: std::sync::Once = std::sync::Once::new();
	WARNED.call_once(|| {
		eprintln!("decoded frame has no colorimetry metadata; assuming sRGB");
	});
}

/// Run a clip's effect stack over its decoded frame (source-first order;
/// disabled effects are bypassed — the C++ traverser's bypass pushes the
/// effect input through unchanged). Effects the montage path cannot
/// evaluate log a warning once and pass the frame through.
fn apply_clip_effects(src: Texture, clip: &crate::ticket::MontageClip, time: Rational) -> Texture {
	apply_effect_list(src, &clip.effects, time)
}

/// Run an effect stack (source-first) over `src`; the clip stacks and the
/// adjustment-layer stacks share this evaluator.
fn apply_effect_list(
	src: Texture,
	effects: &[crate::ticket::MontageEffect],
	time: Rational,
) -> Texture {
	let mut tex = src;
	for effect in effects {
		if !effect.enabled {
			continue;
		}
		tex = apply_montage_effect(tex, effect, time);
	}
	tex
}

/// The factor an Opacity effect would scale its input by: `None` when
/// the effect is not the built-in Opacity or its factor is unity (C++
/// `qFuzzyCompare(opacity, 1.0)`, a pass-through).
fn opacity_factor(effect: &crate::ticket::MontageEffect) -> Option<f32> {
	if effect.type_id != OPACITY_EFFECT_TYPE_ID {
		return None;
	}
	let factor = effect
		.params
		.iter()
		.find(|(id, _)| id == OPACITY_VALUE_INPUT)
		.map(|(_, v)| v.to_double())
		.unwrap_or(1.0);
	if (factor - 1.0).abs() * 1e12 <= factor.abs().min(1.0) {
		return None;
	}
	Some(factor as f32)
}

/// Scale every F32 channel (alpha included) of an F32 RGBA frame by
/// `factor`, in place — the Opacity shader's `frag_color * opacity_in`.
fn scale_channels_in_place(data: &mut [u8], stride: usize, width: i32, height: i32, factor: f32) {
	if stride == 0 || width <= 0 || height <= 0 {
		return;
	}
	let row_bytes = (width as usize) * 16;
	quercus_core::cpuparallel::for_each_row(
		data,
		stride,
		height as usize,
		width as usize,
		|_, row| {
			let row_end = row_bytes.min(row.len());
			for px in row[..row_end].chunks_exact_mut(16) {
				for c in px.chunks_exact_mut(4) {
					let v = f32::from_le_bytes(c.try_into().unwrap());
					c.copy_from_slice(&(v * factor).to_le_bytes());
				}
			}
		},
	);
}

/// Run the GPU Opacity pass on `token`, returning the output token (the
/// caller wraps it in a [`Texture`] whose lease owns it). `None` when the
/// pass cannot run; the caller then passes the input through.
fn apply_opacity_gpu(
	gpu: &quercus_core::backend::GpuContext,
	token: u64,
	width: i32,
	height: i32,
	factor: f32,
) -> Option<u64> {
	let program = gpu
		.compile_shader_pass(OPACITY_GPU_KEY, OPACITY_GPU_WGSL, 1, true, false)
		.ok()?;
	let out = gpu.create_texture(width, height).ok()?;
	match gpu.run_shader_pass(&program, &scale_uniform_bytes(factor), &[token], out) {
		Ok(()) => Some(out),
		Err(_) => {
			gpu.destroy_texture(out);
			None
		}
	}
}

/// Apply one effect to `src` (an F32 RGBA texture of the montage
/// pipeline — GPU-resident on the normal path, a CPU frame on the
/// worker/no-adapter path). `time` is the sequence time the frame is
/// rendered at (the C++ node evaluation time).
fn apply_montage_effect(
	src: Texture,
	effect: &crate::ticket::MontageEffect,
	time: Rational,
) -> Texture {
	// Built-in Opacity: multiply every channel by the opacity factor
	// (C++ `:/shaders/opacity.frag`: `frag_color = texture(tex_in, …) *
	// opacity_in` — the shader scales the whole vec4, alpha included).
	if effect.type_id == OPACITY_EFFECT_TYPE_ID {
		// Unity is a pass-through (C++ `qFuzzyCompare(opacity, 1.0)`).
		let Some(factor) = opacity_factor(effect) else {
			return src;
		};
		// GPU texture: run the same multiply on the GPU. This is a
		// built-in effect, so the montage compositor must not read back
		// for it.
		if matches!(src, Texture::Gpu { .. }) {
			let output = match &src {
				Texture::Gpu {
					token,
					width,
					height,
					ctx,
					..
				} => ctx
					.as_any()
					.and_then(|a| a.downcast_ref::<quercus_core::backend::GpuContext>())
					.and_then(|gpu| apply_opacity_gpu(gpu, *token, *width, *height, factor))
					.map(|out| Texture::gpu(ctx.clone(), out, *width, *height, PixelFormat::F32)),
				_ => None,
			};
			return match output {
				Some(out) => out,
				None => {
					warn_unsupported_once(
						&effect.type_id,
						"opacity GPU pass failed; passing the texture through",
					);
					src
				}
			};
		}
		// Texture implements Drop, so scale in place through a mutable
		// borrow instead of moving the frame out.
		let mut tex = src;
		match &mut tex {
			Texture::Cpu(frame) => {
				let stride = frame.linesize_bytes();
				scale_channels_in_place(&mut frame.data, stride, frame.width, frame.height, factor);
			}
			Texture::Planar(_) => {
				warn_unsupported_once(
					&effect.type_id,
					"opacity on an unresolved planar texture is not supported by the montage path",
				);
			}
			Texture::Gpu { .. } => unreachable!("handled above"),
		}
		return tex;
	}

	// Everything else: an OFX plugin effect. The montage carries the
	// plugin identifier; the rendering process resolves it to a live
	// instance through the quercusplugin-installed factory (lazily created
	// and cached per identifier), then dispatches through the plugin
	// executor exactly like the graph path's plugin jobs. When the single
	// OFX host client is installed the local instance is not needed: the
	// host resolves the identifier itself.
	let instance = if crate::ofxhost::client().is_some() {
		0
	} else {
		let Some(factory) = plugin_instance_factory() else {
			warn_unsupported_once(
				&effect.type_id,
				"no plugin instance factory installed (quercusplugin init missing in this process)",
			);
			return src;
		};
		let Some(instance) = factory(&effect.type_id) else {
			warn_unsupported_once(
				&effect.type_id,
				"no evaluator: unknown built-in effect or OFX plugin unavailable in this process",
			);
			return src;
		};
		instance
	};
	let spec = JobSpec::Plugin {
		instance,
		type_id: effect.type_id.clone(),
		time: time.to_f64(),
		effect_input_id: effect.effect_input_id.clone(),
		inputs: Vec::new(),
		values: effect.params.clone(),
	};
	let size = src.size();
	match RenderEvalHooks::new().process_plugin_job(src, &spec) {
		Ok(texture) => texture,
		Err(err) => {
			// Unreachable for a Plugin spec (the executor failure path
			// yields a purple frame); stay loud rather than silent.
			eprintln!("montage plugin job failed to dispatch: {err:#}");
			purple_frame(time, size)
		}
	}
}

/// The crate-level test lock serializing every test that reads or writes
/// the process-global pipeline color settings
/// ([`quercus_core::color::set_pipeline_color_settings`]). The `pipeline` decode
/// tests pin the legacy working space for the whole decoded-pattern section
/// while the tests below temporarily switch to ACEScg; both modules run in
/// the same test binary, so one shared lock is required.
#[cfg(test)]
pub(crate) fn working_space_test_lock() -> &'static Mutex<()> {
	static LOCK: Mutex<()> = Mutex::new(());
	&LOCK
}

#[cfg(test)]
mod tests;
