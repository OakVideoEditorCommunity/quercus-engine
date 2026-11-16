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

//! The render worker runtime — the Rust port of
//! `engine/src/capi/worker.cpp`, owned by the quercus-worker binary since
//! M14 R2 (the facade keeps its own copy for the frozen
//! `quercusengine_worker_*` C ABI).
//!
//!   - **Backend selection.** [`Renderer::create`] initializes the render
//!     backend through the quercusrender crate's direct Rust API
//!     ([`quercus_core::backend::DisplayRenderer`]), falling back to the
//!     direct OpenGL renderer exactly like the C++ `create_renderer()`
//!     chain. The headless `"cpu"` backend (M15 S1) skips the renderer
//!     entirely — the render path is CPU evaluation + decode, driven
//!     through `render_batch`.
//!   - **The session.** [`WorkerSession`] holds the renderer, the
//!     loaded node graph, the shared-memory frame-slot pools
//!     ([`crate::worker::ipc::FrameSlotPool`]) and the shutdown flag, and answers
//!     one NDJSON control message at a time.
//!   - **The main loop.** [`worker_main`] creates the session, loads the
//!     runtime config (including the quercusplugin render executor), writes
//!     the startup handshake, and serves the stdin/stdout NDJSON loop
//!     until a `shutdown` message or EOF. The worker is single-threaded
//!     by design — there is no render thread: a `render_batch` renders
//!     its tickets synchronously on this loop thread, and parallelism
//!     comes from the main process's worker pool
//!     (`quercus_render::procpool`), not from threads here (see "Where the
//!     rendering happens" in this crate's README).
//!
//! Real rendering landed in M15 S1: `load_graph` deserializes the graph
//! snapshot file (quercusnode project XML, with the minimal
//! `{"project_copy":N}` payload fallback); `render_frame` and
//! `render_batch` render through [`quercus_render::eval`] (generated frames,
//! footage decode, montage compositing) directly into the main-assigned
//! shm slots and publish `frame_ready` / `frame_failed` (protocol v2).

use quercus_core::TimeRange;
use quercus_core::color;
use quercus_core::colormath;
use quercus_core::debug;
use quercus_core::frame;
use quercus_core::texture;
use quercus_node::id;
use quercus_node::project;
use quercus_node::serializer;
use quercus_ofx_plugin::host;
use quercus_ofx_plugin::node_factory;
use quercus_ofx_plugin::progress;
use quercus_render::textengine;
use std::collections;
use std::env;
use std::fs;
use std::mem;
use std::panic;
use std::path;
use std::process;
use std::ptr;
use std::slice;
use std::thread;
mod framecache;
mod ipc;
pub(crate) mod ofx_host;
#[cfg(test)]
mod process_tests;
#[cfg(test)]
mod procpool_integration;

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use quercus_core::backend::{BackendKind, DisplayRenderer};
use quercus_core::{PixelFormat, Rational};
use quercus_render::eval;
use quercus_render::ticket::{AdjustmentSpan, AudioTicketParams, MontageClip, VideoTicketParams};

use crate::worker::framecache::FrameCache;
use crate::worker::ipc::{
	AudioTicketSpec, BatchTicketSpec, FrameSlotMeta, FrameSlotPool, HandshakeMsg, LoadGraphMsg,
	PluginProgressMsg, RenderAudioBatchMsg, RenderBatchMsg, RenderFrameMsg, SLOT_FORMAT_AUDIO_F32,
	SLOT_FORMAT_BGRA8, SharedMemoryRegion, ShmMode, TYPE_CANCEL, TYPE_HANDSHAKE, TYPE_LOAD_GRAPH,
	TYPE_PLUGIN_CANCEL, TYPE_RENDER_AUDIO_BATCH, TYPE_RENDER_BATCH, TYPE_RENDER_FRAME,
	TYPE_SHUTDOWN, error_message, write_message,
};

/// Protocol version announced in the startup handshake.
pub const PROTOCOL_VERSION: i32 = 1;

/// Log a worker-side error through the engine log (the `worker: ` prefix,
/// so an installed sink sees worker diagnostics alongside the engine's).
pub fn log_error(message: &str) {
	let _ = debug::log(debug::Level::Error, &format!("worker: {message}"));
}

/// Serializes unit tests that install or drive the process-global
/// plugin/progress factories (the `ofx_host` tests share this lock: the
/// worker's and the host's reporter factories are process-wide).
#[cfg(test)]
pub(crate) static GLOBAL_FACTORY_TEST_LOCK: Mutex<()> = Mutex::new(());

/// A loaded graph snapshot (M15 S1): the snapshot file path plus what it
/// deserialized into — a full quercusnode project, or only the copied-project
/// identity (the minimal `{"project_copy":N}` payload the
/// [`quercus_render::worker::GraphSnapshotStore`] wrote before M16 S1 shipped
/// full graph snapshots).
struct LoadedGraph {
	/// Snapshot file path (S2: graph_update diffing is path-based).
	#[allow(dead_code)]
	path: String,
	/// The deserialized project (M16 S1: node-graph tickets render the
	/// viewer's frame from here; montage/footage/generate tickets use the
	/// loaded context too).
	project: Option<Arc<Mutex<project::Project>>>,
	/// The loaded snapshot's owning project uuid; `None` for the
	/// identity-only legacy payload. Graph-mode tickets (which carry their
	/// owning project's uuid) only render from this graph when it matches —
	/// a stale snapshot from a different project must not answer foreign
	/// viewer identities (M16 S1 cross-process snapshot race).
	project_uuid: Option<String>,
	/// Source-identity -> loaded-id map (see
	/// [`quercus_node::serializer::load_with_id_map`]): tickets carry viewer
	/// identities from the *saved* project, while the loaded graph's arena
	/// slots differ (deleted slots shift every later node).
	id_map: collections::HashMap<u64, id::NodeId>,
	project_copy: u64,
}

// ---------------------------------------------------------------------------
// Worker-side plugin-progress forwarding
// ---------------------------------------------------------------------------
//
// OFX plugin rendering happens in this process (crash isolation), so the
// main process's inline progress reporter is not in effect here. The
// worker installs its own progress reporter factory (see
// [`install_worker_progress_factory`]) whose reporters push `plugin_progress`
// NDJSON events into [`WORKER_PROGRESS_EVENTS`]; the main loop drains the
// buffer after each control message ([`flush_worker_progress`]). Plugin
// renders are synchronous on the loop thread, so the buffer only ever
// mutates there — a plain `Mutex` suffices.
//
// Cancel: the main process broadcasts `plugin_cancel` (the progress
// dialog's Cancel button); the worker sets [`WORKER_PLUGIN_CANCEL`] and
// every live reporter answers false (the plugin aborts at its next
// progressUpdate). Mirrors the main-process reporter factory: a fresh
// render (progressStart) resets the sticky flag. Because the worker
// processes control messages between batches, an in-flight frame
// completes before the cancel is observed (batch granularity); the main
// process stops the render loop separately (export cancel atom / preview
// window invalidation).

/// The worker's sticky plugin-cancel flag (set by the `plugin_cancel`
/// control message, read by every live progress reporter).
static WORKER_PLUGIN_CANCEL: AtomicBool = AtomicBool::new(false);

/// Buffered `plugin_progress` events awaiting the next flush.
static WORKER_PROGRESS_EVENTS: Mutex<Vec<Value>> = Mutex::new(Vec::new());

/// Queue one `plugin_progress` NDJSON event for the main process.
///
/// OFX plugin rendering runs in the worker process, so plugin progress is
/// forwarded instead of shown: the event is serialized now and pushed onto the
/// shared buffer that the main loop drains after each control message (see
/// `flush_worker_progress`). Called from the loop thread only, which makes the
/// plain mutex sufficient; a poisoned lock is recovered rather than failing.
///
/// # Arguments
///
/// * `fraction` - Progress in the range 0.0..=1.0 (1.0 signals completion).
/// * `label` - Plugin/step label shown next to the progress bar.
/// * `message` - Detail line for the progress dialog; both strings are copied
///   into the event.
fn push_worker_progress(fraction: f64, label: &str, message: &str) {
	let event = PluginProgressMsg {
		label: label.to_string(),
		message: message.to_string(),
		fraction,
	}
	.to_json();
	WORKER_PROGRESS_EVENTS
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.push(event);
}

/// Write every buffered progress event to `out` (called by the main loop
/// after each control message; `out` is the NDJSON stdout writer).
fn flush_worker_progress(out: &mut impl Write) {
	let events = mem::take(
		&mut *WORKER_PROGRESS_EVENTS
			.lock()
			.unwrap_or_else(|e| e.into_inner()),
	);
	for event in events {
		if write_message(out, &event).is_err() {
			break;
		}
	}
	let _ = out.flush();
}

/// The worker-side `UiProgressReporter`: forwards (label, message,
/// fraction) to the main process and honours the sticky cancel flag.
struct WorkerProgressReporter {
	label: String,
	message: String,
}

impl progress::UiProgressReporter for WorkerProgressReporter {
	fn update(&mut self, progress: f64) -> bool {
		push_worker_progress(progress, &self.label, &self.message);
		!WORKER_PLUGIN_CANCEL.load(Ordering::Relaxed)
	}

	fn end(&mut self) {
		// progressEnd: forward completion (fraction 1.0) so the app closes
		// the progress dialog without waiting for a 1.0 update.
		push_worker_progress(1.0, &self.label, &self.message);
	}
}

/// Install the worker-side progress reporter factory. Called from
/// [`WorkerSession::initialize_runtime`]; mirrors the main-process factory
/// (a fresh progressStart resets the sticky cancel flag).
fn install_worker_progress_factory() {
	progress::set_reporter_factory(Some(Arc::new(|label, message| {
		// A fresh render begins: reset the sticky cancel flag.
		WORKER_PLUGIN_CANCEL.store(false, Ordering::Relaxed);
		push_worker_progress(0.0, label, message);
		Box::new(WorkerProgressReporter {
			label: label.to_string(),
			message: message.to_string(),
		})
	})));
}

// ---------------------------------------------------------------------------
// Renderer (backend selection)
// ---------------------------------------------------------------------------

/// Whether `backend` requests no renderer (worker.cpp
/// `backend_requests_no_renderer()`: NULL, "" and "none").
pub fn is_no_backend(backend: &str) -> bool {
	backend.is_empty() || backend.eq_ignore_ascii_case("none")
}

/// Whether `backend` is the M15 headless CPU render mode: like "none"
/// (no GPU renderer) but the session stays fully operational — frames
/// render through the CPU evaluation path ([`quercus_render::eval`]).
pub fn is_cpu_backend(backend: &str) -> bool {
	backend.eq_ignore_ascii_case("cpu")
}

/// A live, initialized quercusrender display renderer (destroyed on drop).
pub struct Renderer {
	/// The quercusrender crate's value-typed display renderer (single-lib
	/// unification; the CHandle-based C ABI is deleted).
	inner: DisplayRenderer,
}

impl Renderer {
	/// Create and initialize a renderer through the quercusrender crate's
	/// direct Rust API, trying the named dynamic backend first and falling
	/// back to the direct OpenGL renderer — the exact fallback chain of
	/// worker.cpp `create_renderer()`.
	pub fn create(backend: &str) -> Result<Renderer, String> {
		match Self::create_dynamic(backend) {
			Ok(r) => Ok(r),
			Err(first) => {
				log_error(&format!(
					"failed to initialize dynamic {backend} backend: {first}; falling back to direct OpenGL renderer"
				));
				Self::create_opengl().map_err(|second| {
					format!("{first}; direct OpenGL fallback also failed: {second}")
				})
			}
		}
	}

	/// Try the named dynamic backend (`DisplayRenderer::new` +
	/// `init`, the single-lib equivalent of
	/// `quercusrender_display_renderer_create_dynamic` + `_init`).
	fn create_dynamic(backend: &str) -> Result<Renderer, String> {
		let renderer = DisplayRenderer::new(BackendKind::from_config_string(backend));
		Self::init_inner(renderer, &format!("dynamic {backend}"))
	}

	/// Fall back to the direct OpenGL renderer.
	fn create_opengl() -> Result<Renderer, String> {
		let renderer = DisplayRenderer::new(BackendKind::Gl);
		Self::init_inner(renderer, "direct OpenGL")
	}

	/// Initialize a freshly created display renderer and wrap it in a
	/// [`Renderer`].
	///
	/// `renderer` is the value returned by `DisplayRenderer::new` for the
	/// backend chosen by `create` (dynamic backend or the direct OpenGL
	/// fallback). `what` is only a human-readable label for the error message
	/// (`"dynamic vulkan"`, `"direct OpenGL"`), not a backend selector. Init is
	/// called with a null GL context so the backend uses its default
	/// device/context path — the same call the C++ `create_renderer()` makes.
	///
	/// # Errors
	///
	/// Returns `"failed to initialize {what} renderer ({e})"` when
	/// `DisplayRenderer::init` fails; `Renderer::create` logs that message and
	/// then retries the direct OpenGL backend.
	fn init_inner(mut renderer: DisplayRenderer, what: &str) -> Result<Renderer, String> {
		// NULL gl_context makes the backend use its default device/context
		// path.
		if let Err(e) = renderer.init(ptr::null_mut()) {
			return Err(format!("failed to initialize {what} renderer ({e})"));
		}
		Ok(Renderer { inner: renderer })
	}

	/// 1 when the renderer is OpenGL-based (the C++ worker uses the GL
	/// context to announce the negotiated GL version in the handshake).
	///
	/// Not called yet: the quercusrender module exposes no GL context
	/// version, so the startup handshake omits `gl_major`/`gl_minor`.
	#[allow(dead_code)]
	pub fn is_open_gl(&self) -> bool {
		self.inner.is_open_gl()
	}
}

// ---------------------------------------------------------------------------
// WorkerSession
// ---------------------------------------------------------------------------

/// The worker-side session state machine — the Rust mirror of
/// `QuercusWorkerSession` in worker.cpp. Holds the renderer, the attached
/// shared-memory frame-slot pools and the shutdown flag, and answers one
/// NDJSON control message at a time.
pub struct WorkerSession {
	renderer: Option<Renderer>,
	shutdown_requested: bool,
	runtime_initialized: bool,
	output_region: Option<SharedMemoryRegion>,
	output_pool: Option<FrameSlotPool>,
	input_region: Option<SharedMemoryRegion>,
	input_pool: Option<FrameSlotPool>,
	graph: Option<LoadedGraph>,
	/// Reusable F32 staging buffer (BGRA8 slot conversion; the F32
	/// pipeline renders there before the end-of-pipe format convert).
	f32_scratch: Vec<u8>,
	/// LRU byte-budgeted memo of rendered F32 frames, keyed by the
	/// render-deterministic ticket-spec subset (M16 S2; see
	/// [`crate::worker::framecache`]).
	frame_cache: FrameCache,
}

impl WorkerSession {
	/// Create a session for `backend`, mirroring
	/// `quercusengine_worker_session_create()`: "none"/"" skips renderer
	/// creation, anything else initializes the render backend through the
	/// quercusrender crate's direct Rust API (dynamic -> OpenGL fallback).
	/// The M15 `"cpu"` backend is the headless render mode: no renderer,
	/// CPU evaluation + decode via [`quercus_render::eval`]. M16 S1: a failed
	/// renderer init for any other backend (e.g. "auto" on a GPU-less
	/// host) is tolerated — the session continues headless and tickets
	/// evaluate through the CPU path.
	pub fn create(backend: &str) -> Result<WorkerSession, String> {
		let renderer = match backend {
			b if is_no_backend(b) || is_cpu_backend(b) => None,
			b => match Renderer::create(b) {
				Ok(r) => Some(r),
				Err(e) => {
					log_error(&format!(
						"session: {b} renderer init failed ({e}); continuing headless (CPU eval path)"
					));
					None
				}
			},
		};
		Ok(WorkerSession {
			renderer,
			shutdown_requested: false,
			runtime_initialized: false,
			output_region: None,
			output_pool: None,
			input_region: None,
			input_pool: None,
			graph: None,
			f32_scratch: Vec::new(),
			frame_cache: FrameCache::new(),
		})
	}

	/// 1 when the session holds a successfully initialized render backend.
	pub fn has_renderer(&self) -> bool {
		self.renderer.is_some()
	}

	/// 1 once a shutdown control message has been received.
	pub fn shutdown_requested(&self) -> bool {
		self.shutdown_requested
	}

	/// Load the runtime services the session depends on — the Rust analog
	/// of the C++ `initialize_runtime()`. Of the C++ list (config, node
	/// factory, color manager, frame/disk managers, project serializer)
	/// only the color-manager default config has a Rust backing linked into
	/// the worker binary; the rest are logged and skipped. Always returns
	/// true (the C++ returns true unconditionally).
	pub fn initialize_runtime(&mut self) -> bool {
		if self.runtime_initialized {
			return true;
		}
		log_error("runtime: loading color-manager default config");
		if let Err(e) = color::set_up_default_config() {
			log_error(&format!(
				"runtime: color-manager default config failed ({e}); continuing"
			));
		}
		// The text nodes rasterize through the textbackend hooks: the pool
		// renders text clips too, so every worker installs the same
		// cosmic-text engine as the UI process (installing it only there
		// left text invisible in previews — the hooks are per-process).
		log_error("runtime: installing text layout backends");
		textengine::install();
		// M15 S1: the plugin execution stack lives in the worker process
		// (OFX crashes take down this process, not the editor — design
		// §3.6). quercusplugin installs its render driver into the quercusrender
		// executor slot.
		log_error("runtime: installing quercusplugin render executor");
		node_factory::install_render_executor();
		// M15 S2: graphs carrying OFX plugin nodes deserialize/evaluate in
		// the worker too, so the per-process node factory must register the
		// discovered plugins exactly like the main process. A failed scan is
		// non-fatal: the worker stays up for plugin-free graphs.
		log_error("runtime: scanning and registering OFX plugins");
		if let Err(e) = host::Host::global().cache.scan() {
			log_error(&format!(
				"runtime: OFX plugin scan failed ({e}); continuing"
			));
		}
		let discovered = host::Host::global().cache.count();
		let registered = node_factory::register_plugin_nodes();
		log_error(&format!(
			"runtime: discovered {} OFX plugin(s), registered {} node type(s)",
			discovered,
			registered.len()
		));
		// Worker-side plugin progress forwarding (see the module docs): the
		// plugin progress suite runs in this process, so progress events
		// must cross the IPC boundary to reach the app's progress dialog.
		log_error("runtime: installing worker plugin-progress reporter factory");
		install_worker_progress_factory();
		log_error(
			"runtime: config / frame manager / disk manager / project \
			 serializer have no Rust backing in the worker binary; skipped",
		);
		self.runtime_initialized = true;
		true
	}

	/// The startup handshake the worker sends to its parent
	/// (`worker.cpp startup_handshake()`): protocol version 1 and empty
	/// shared-memory geometry — the parent creates the segments and
	/// announces their geometry in its handshake reply.
	///
	/// Deviation from the C++: `gl_major`/`gl_minor` are omitted because
	/// the quercusrender module exposes no GL context version.
	pub fn startup_handshake(&self) -> Value {
		HandshakeMsg {
			protocol_version: PROTOCOL_VERSION,
			shm_key: String::new(),
			input_shm_key: String::new(),
			input_slots: 0,
			output_slots: 0,
			slot_data_bytes: 0,
			input_slot_data_bytes: 0,
		}
		.to_json()
	}

	/// Handle one complete NDJSON control line and produce the response, if
	/// any — the port of worker.cpp `handle()`. A malformed line yields an
	/// error response (the loop continues), never a failure.
	pub fn handle_line(&mut self, line: &str) -> Option<Value> {
		let msg: Value = match serde_json::from_str::<Value>(line) {
			Ok(v) if v.is_object() => v,
			_ => return Some(error_message("malformed control message", None)),
		};
		let typ = msg.get("type").and_then(Value::as_str).unwrap_or("");
		match typ {
			TYPE_HANDSHAKE => self.handle_handshake(&msg),
			TYPE_LOAD_GRAPH => self.handle_load_graph(&msg),
			TYPE_RENDER_FRAME => self.handle_render_frame(&msg),
			// cancel: the worker does synchronous single-frame work
			// (nothing in flight), so a cancel produces no response.
			TYPE_CANCEL => None,
			// plugin_cancel: the user cancelled the plugin render; sticky
			// until the next progressStart resets it (main-process
			// semantics).
			TYPE_PLUGIN_CANCEL => {
				WORKER_PLUGIN_CANCEL.store(true, Ordering::Relaxed);
				None
			}
			TYPE_SHUTDOWN => {
				self.shutdown_requested = true;
				None
			}
			other => Some(error_message(
				&format!("unknown message type: {other}"),
				None,
			)),
		}
	}

	/// `handshake`: validate and attach the shared-memory frame-slot pools
	/// — the real port of worker.cpp `attach_output_pool()`.
	fn handle_handshake(&mut self, msg: &Value) -> Option<Value> {
		let hs: HandshakeMsg = match serde_json::from_value(msg.clone()) {
			Ok(hs) => hs,
			Err(_) => return Some(error_message("invalid handshake message", None)),
		};
		if hs.protocol_version != PROTOCOL_VERSION {
			return Some(error_message(
				&format!("unsupported protocol version {}", hs.protocol_version),
				None,
			));
		}
		if hs.shm_key.is_empty() || hs.output_slots <= 0 || hs.slot_data_bytes <= 0 {
			return Some(error_message(
				"handshake missing output shared-memory geometry",
				None,
			));
		}

		// A re-handshake replaces the pools (worker.cpp resets the input
		// pool before attaching the output).
		self.input_pool = None;
		self.input_region = None;
		self.output_pool = None;
		self.output_region = None;

		let bytes =
			FrameSlotPool::bytes_needed(hs.output_slots as u32, hs.slot_data_bytes as usize);
		let mut output_region = SharedMemoryRegion::new();
		if !output_region.open(&hs.shm_key, bytes, ShmMode::Attach) {
			return Some(error_message(
				&format!("failed to attach shared memory: {}", output_region.error()),
				None,
			));
		}
		// SAFETY: `output_region` is a live mapping of at least `bytes`
		// bytes (checked above).
		let output_pool = unsafe { FrameSlotPool::attach(output_region.data()) };
		if !output_pool.is_valid() {
			return Some(error_message(
				"shared memory does not contain a frame slot pool",
				None,
			));
		}
		self.output_region = Some(output_region);
		self.output_pool = Some(output_pool);

		if hs.input_slots > 0 {
			if hs.input_shm_key.is_empty() || hs.input_slot_data_bytes <= 0 {
				return Some(error_message(
					"handshake missing input shared-memory geometry",
					None,
				));
			}
			let input_bytes = FrameSlotPool::bytes_needed(
				hs.input_slots as u32,
				hs.input_slot_data_bytes as usize,
			);
			let mut input_region = SharedMemoryRegion::new();
			if !input_region.open(&hs.input_shm_key, input_bytes, ShmMode::Attach) {
				return Some(error_message(
					&format!(
						"failed to attach input shared memory: {}",
						input_region.error()
					),
					None,
				));
			}
			// SAFETY: `input_region` is a live mapping of at least
			// `input_bytes` bytes (checked above).
			let input_pool = unsafe { FrameSlotPool::attach(input_region.data()) };
			if !input_pool.is_valid() {
				return Some(error_message(
					"input shared memory does not contain a frame slot pool",
					None,
				));
			}
			self.input_region = Some(input_region);
			self.input_pool = Some(input_pool);
		}

		// Success: protocol v2 answers the geometry handshake with the
		// capability announcement (worker.cpp left the response empty;
		// main now waits for hello_caps to mark the worker alive).
		Some(json!({
			"type": crate::worker::ipc::TYPE_HELLO_CAPS,
			"protocol_version": PROTOCOL_VERSION,
			"formats": [PixelFormat::F32 as i32, SLOT_FORMAT_BGRA8, SLOT_FORMAT_AUDIO_F32],
			"max_slot_bytes": hs.slot_data_bytes,
		}))
	}

	/// `load_graph` (M15 S1): the file checks mirror worker.cpp; the
	/// payload is deserialized for real — an quercusnode project XML
	/// ([`quercus_node::serializer::load`]) or the minimal
	/// `{"project_copy":N}` identity payload written by the snapshot
	/// store before full graph uploads land in S2. Success answers
	/// nothing (v1 semantics); failures answer an `error` message.
	fn handle_load_graph(&mut self, msg: &Value) -> Option<Value> {
		let load: LoadGraphMsg = match serde_json::from_value(msg.clone()) {
			Ok(l) => l,
			Err(_) => return Some(error_message("invalid load_graph message", None)),
		};
		match fs::metadata(&load.path) {
			Err(_) => Some(error_message(
				&format!("graph file does not exist: {}", load.path),
				None,
			)),
			Ok(md) if md.len() == 0 => Some(error_message(
				&format!("graph file is empty: {}", load.path),
				None,
			)),
			Ok(md) => {
				log_error(&format!(
					"LoadGraph: loading {} ({} bytes)",
					load.path,
					md.len()
				));
				let content = match fs::read_to_string(&load.path) {
					Ok(c) => c,
					Err(e) => {
						return Some(error_message(&format!("graph file unreadable: {e}"), None));
					}
				};
				match serializer::load_with_id_map(&content) {
					Ok((project, id_map)) => {
						let (working, output_spec) = {
							let guard = project.lock().unwrap_or_else(|e| e.into_inner());
							(guard.working_color_space(), guard.output_color_spec())
						};
						let project_uuid = project
							.lock()
							.unwrap_or_else(|e| e.into_inner())
							.uuid
							.clone();
						self.graph = Some(LoadedGraph {
							path: load.path.clone(),
							project: Some(project),
							project_uuid: Some(project_uuid),
							id_map,
							project_copy: 0,
						});
						// The project's color pipeline properties drive this
						// process's input/output transforms (the quercusrender
						// process global read by eval + the output node).
						color::set_pipeline_color_settings(working, output_spec);
						// A fresh graph snapshot can change what any viewer
						// identity renders; cached pixels from the previous
						// graph must not be served (M16 S2 frame cache).
						self.frame_cache.clear();
						log_error("LoadGraph: quercusnode project deserialized");
						None
					}
					Err(graph_err) => {
						// Fallback: the snapshot store's minimal payload
						// `{"project_copy":N}` (identity-only graph context).
						if let Ok(v) = serde_json::from_str::<Value>(&content) {
							if let Some(pc) = v.get("project_copy").and_then(Value::as_u64) {
								self.graph = Some(LoadedGraph {
									path: load.path.clone(),
									project: None,
									project_uuid: None,
									id_map: collections::HashMap::new(),
									project_copy: pc,
								});
								// See the full-snapshot branch: any reload
								// invalidates the frame cache (M16 S2).
								self.frame_cache.clear();
								log_error(&format!(
									"LoadGraph: identity-only snapshot (project_copy {pc})"
								));
								return None;
							}
						}
						Some(error_message(
							&format!("graph deserialization failed: {graph_err}"),
							None,
						))
					}
				}
			}
		}
	}

	/// `render_frame` (v1 single-frame path, M15 S1 real): generate the
	/// frame through [`quercus_render::eval`], write it into an acquired shm
	/// slot and answer `frame_ready`. The v1 message carries no montage
	/// or footage fields, so this path renders the pipeline's generated
	/// frame; montage/footage tickets arrive via `render_batch`.
	fn handle_render_frame(&mut self, msg: &Value) -> Option<Value> {
		let render: RenderFrameMsg = match serde_json::from_value(msg.clone()) {
			Ok(r) => r,
			Err(_) => return Some(error_message("invalid render_frame message", None)),
		};
		let pool = match self.output_pool.as_ref() {
			Some(p) if p.is_valid() => p,
			_ => {
				return Some(error_message(
					"render_frame: no shared-memory pool attached",
					Some(render.ticket),
				));
			}
		};
		let (w, h) = if render.width > 0 && render.height > 0 {
			(render.width, render.height)
		} else {
			(
				frame::VideoParamsPod::DEFAULT_WIDTH,
				frame::VideoParamsPod::DEFAULT_HEIGHT,
			)
		};
		let format = if render.format < 0 {
			PixelFormat::F32
		} else {
			match render.format {
				f if f == PixelFormat::U8 as i32 => PixelFormat::U8,
				f if f == PixelFormat::U10 as i32 => PixelFormat::U10,
				f if f == PixelFormat::U16 as i32 => PixelFormat::U16,
				f if f == PixelFormat::F16 as i32 => PixelFormat::F16,
				f if f == PixelFormat::F32 as i32 => PixelFormat::F32,
				other => {
					return Some(error_message(
						&format!("render_frame: unsupported format {other}"),
						Some(render.ticket),
					));
				}
			}
		};
		let time = Rational::new(render.time_num, render.time_den);
		let frame = match eval::generate_frame(time, (w, h), format) {
			Ok(f) => f,
			Err(e) => {
				return Some(error_message(
					&format!("render_frame: generation failed: {e}"),
					Some(render.ticket),
				));
			}
		};

		let slot = match self.acquire_slot(pool) {
			Some(s) => s,
			None => {
				return Some(error_message(
					"render_frame: no free shm slot",
					Some(render.ticket),
				));
			}
		};
		// Write meta + pixels into the slot, then publish.
		let data_size = frame.data.len();
		if data_size > pool.slot_data_bytes() {
			return Some(error_message(
				"render_frame: frame larger than the shm slot",
				Some(render.ticket),
			));
		}
		// SAFETY: `slot` was acquired; the slot block and meta are live
		// shared memory of the attached pool.
		unsafe {
			ptr::copy_nonoverlapping(frame.data.as_ptr(), pool.slot_data(slot), data_size);
			let meta = &mut *pool.meta(slot);
			*meta = FrameSlotMeta::default();
			meta.id = render.ticket;
			meta.time_num = render.time_num;
			meta.time_den = render.time_den;
			meta.width = w;
			meta.height = h;
			meta.format = format as i32;
			meta.channel_count = 4;
			meta.linesize = frame.linesize_bytes() as i32;
			meta.data_size = data_size as i32;
			if !pool.publish(slot) {
				return Some(error_message(
					"render_frame: ready ring full",
					Some(render.ticket),
				));
			}
		}
		Some(json!({
			"type": crate::worker::ipc::TYPE_FRAME_READY,
			"ticket": render.ticket,
			"slot": slot,
		}))
	}

	/// `render_batch` (protocol v2; M15 S1): claim confirmation followed
	/// by in-order rendering of every ticket into its main-assigned shm
	/// slot. Responses stream to `out`: one `batch_accepted`, then one
	/// `frame_ready` or `frame_failed` per ticket. Crashes the process
	/// deliberately when the crash-mode environment asks for it (the
	/// crash-isolation test hook).
	fn handle_render_batch_stream(&mut self, line: &str, out: &mut impl Write) -> io::Result<()> {
		let batch: RenderBatchMsg = match serde_json::from_str(line) {
			Ok(b) => b,
			Err(_) => {
				return write_message(out, &error_message("invalid render_batch message", None));
			}
		};

		// Explicit claim confirmation (design §3.3): these tickets are
		// owned by this worker now — no work stealing. The `type` tag is
		// built by hand because [`BatchAcceptedMsg`] only carries the
		// payload fields.
		let accepted = json!({
			"type": crate::worker::ipc::TYPE_BATCH_ACCEPTED,
			"batch_id": batch.batch_id,
			"tickets": batch.tickets.iter().map(|t| t.ticket).collect::<Vec<_>>(),
		});
		write_message(out, &accepted)?;
		out.flush()?;

		for spec in &batch.tickets {
			// Crash-isolation test hook: QUERCUS_WORKER_CRASH_ON_TICKET=<n>
			// segfaults while rendering ticket n. A marker file (env
			// QUERCUS_WORKER_CRASH_MARKER) makes the crash one-shot so the
			// restarted worker renders the frame for real.
			self.maybe_crash_for_testing(spec.ticket);

			let response = match self.render_ticket_to_slot(spec) {
				Ok(slot) => json!({
					"type": crate::worker::ipc::TYPE_FRAME_READY,
					"ticket": spec.ticket,
					"slot": slot,
				}),
				Err(e) => {
					log_error(&format!("render_batch: ticket {} failed: {e}", spec.ticket));
					json!({
						"type": crate::worker::ipc::TYPE_FRAME_FAILED,
						"ticket": spec.ticket,
						"error": e,
					})
				}
			};
			write_message(out, &response)?;
			out.flush()?;
		}
		Ok(())
	}

	/// Deliberately crash the worker process when the crash-isolation test hook
	/// asks for this ticket.
	///
	/// Reads `QUERCUS_WORKER_CRASH_ON_TICKET` (an `i64` ticket number) and
	/// `QUERCUS_WORKER_CRASH_MARKER` (a file path) from the environment on every
	/// call, so a test can arm/disarm the hook per process. It crashes only when
	/// the variable is set, parses, and equals `ticket`; when a marker path is
	/// given and the file already exists the hook is considered spent and the
	/// call returns, which makes the crash one-shot so the restarted worker
	/// renders the frame for real. Otherwise it writes the marker, raises
	/// `SIGSEGV` (to look like a real plugin crash) and falls back to
	/// `std::process::abort`, so it never returns once it decides to crash.
	///
	/// `ticket` is the wire ticket id of the frame about to be rendered. This
	/// runs before every render in the video and audio batch loops and is a
	/// no-op in production, where neither variable is set.
	fn maybe_crash_for_testing(&self, ticket: i64) {
		let Ok(want) = env::var("QUERCUS_WORKER_CRASH_ON_TICKET") else {
			return;
		};
		let Ok(crash_on) = want.parse::<i64>() else {
			return;
		};
		if crash_on != ticket {
			return;
		}
		let marker = env::var("QUERCUS_WORKER_CRASH_MARKER").ok();
		let should_crash = match &marker {
			Some(path) => !path::Path::new(path).exists(),
			None => true,
		};
		if !should_crash {
			return;
		}
		if let Some(path) = &marker {
			let _ = fs::write(path, b"crashed");
		}
		log_error(&format!("crash mode: dying on ticket {ticket}"));
		// Raise SIGSEGV like a real plugin crash; abort as the fallback.
		unsafe { libc::raise(libc::SIGSEGV) };
		process::abort();
	}

	/// Acquire a free output slot, polling until one appears (the free
	/// ring is the only filler-side entry point; flow control). `None`
	/// on shutdown or after the 30 s safety deadline.
	fn acquire_slot(&self, pool: &FrameSlotPool) -> Option<u32> {
		let deadline = Instant::now() + Duration::from_secs(30);
		let mut slot = 0u32;
		loop {
			if self.shutdown_requested {
				return None;
			}
			// SAFETY: valid attached pool; the worker is the filler, so
			// popping the free ring is its SPSC role.
			if unsafe { pool.acquire(&mut slot) } {
				return Some(slot);
			}
			if Instant::now() > deadline {
				return None;
			}
			thread::sleep(Duration::from_millis(1));
		}
	}

	/// Render one batch ticket into its main-assigned output slot.
	///
	/// Clones the pool view (cheap, shared mapping) so the render path can still
	/// borrow `self` mutably for its scratch buffer and frame cache, acquires a
	/// slot from the free ring, verifies that the popped slot equals `spec.slot`
	/// (main seeds the ring in assignment order, so anything else is a protocol
	/// violation), renders the pixels via `render_spec_pixels`, and publishes the
	/// slot to the ready ring.
	///
	/// # Returns
	///
	/// The published slot index, i.e. `spec.slot`.
	///
	/// # Errors
	///
	/// Returns a printable message when no valid output pool is attached, when
	/// the acquire times out or shutdown was requested, when the popped slot does
	/// not match the assignment, when rendering fails (the message is prefixed by
	/// the producer, e.g. `footage decode: ...`), or when the ready ring is full
	/// at publish time. On error the slot is not published; the main process
	/// recycles it after seeing `frame_failed`.
	fn render_ticket_to_slot(&mut self, spec: &BatchTicketSpec) -> Result<u32, String> {
		// Clone the pool view (a cheap mapping-shared copy) so the render
		// path below can borrow `self` mutably (scratch buffer).
		let pool = match self.output_pool.clone() {
			Some(p) if p.is_valid() => p,
			_ => return Err("no shared-memory pool attached".to_string()),
		};

		// Flow control: acquire through the free ring. Main seeds and
		// releases slots in assignment order, so the pop yields exactly
		// the assigned slot — anything else is a protocol violation.
		let acquired = self
			.acquire_slot(&pool)
			.ok_or_else(|| "no free shm slot (shutdown or timeout)".to_string())?;
		if acquired != spec.slot as u32 {
			return Err(format!(
				"slot assignment mismatch: acquired {acquired}, assigned {}",
				spec.slot
			));
		}
		let slot = acquired;

		let result = self.render_spec_pixels(spec, &pool);
		match result {
			Ok(()) => {
				// SAFETY: `slot` was acquired above and rendered into.
				let published = unsafe { pool.publish(slot) };
				if !published {
					Err("ready ring full".to_string())
				} else {
					Ok(slot)
				}
			}
			// The slot was acquired but never published; main recycles it
			// when it sees frame_failed (the worker cannot push back to
			// the free ring — that is the drainer's SPSC role).
			Err(e) => Err(e),
		}
	}

	/// `render_audio_batch` (protocol v2, M15 S3): claim confirmation
	/// followed by in-order mixing of every audio range pull into its
	/// main-assigned shm slot (interleaved f32, wire format
	/// [`SLOT_FORMAT_AUDIO_F32`]). Responses stream to `out`: one
	/// `batch_accepted`, then one `frame_ready` or `frame_failed` per
	/// ticket — the same claim/credit/frame_ready flow as
	/// [`Self::handle_render_batch_stream`]. Crashes the process
	/// deliberately when the crash-mode environment asks for it (the
	/// audio crash-isolation test hook; the audio slot geometry check is
	/// below the video one in `handle_line`).
	fn handle_render_audio_batch_stream(
		&mut self,
		line: &str,
		out: &mut impl Write,
	) -> io::Result<()> {
		let batch: RenderAudioBatchMsg = match serde_json::from_str(line) {
			Ok(b) => b,
			Err(_) => {
				return write_message(
					out,
					&error_message("invalid render_audio_batch message", None),
				);
			}
		};

		let accepted = json!({
			"type": crate::worker::ipc::TYPE_BATCH_ACCEPTED,
			"batch_id": batch.batch_id,
			"tickets": batch.tickets.iter().map(|t| t.ticket).collect::<Vec<_>>(),
		});
		write_message(out, &accepted)?;
		out.flush()?;

		for spec in &batch.tickets {
			self.maybe_crash_for_testing(spec.ticket);

			let response = match self.render_audio_ticket_to_slot(spec) {
				Ok(slot) => json!({
					"type": crate::worker::ipc::TYPE_FRAME_READY,
					"ticket": spec.ticket,
					"slot": slot,
				}),
				Err(e) => {
					log_error(&format!(
						"render_audio_batch: ticket {} failed: {e}",
						spec.ticket
					));
					json!({
						"type": crate::worker::ipc::TYPE_FRAME_FAILED,
						"ticket": spec.ticket,
						"error": e,
					})
				}
			};
			write_message(out, &response)?;
			out.flush()?;
		}
		Ok(())
	}

	/// Mix one audio range pull into its main-assigned shared-memory slot.
	///
	/// The flow mirrors `render_ticket_to_slot`: acquire a slot from the free
	/// ring, check it is the one `spec.slot` assigned, convert the spec to eval
	/// audio params, verify that the range's byte length fits the slot, mix
	/// interleaved F32 samples into the slot's data block and publish it. The meta
	/// is written using the video field layout, with `width` holding the sample
	/// rate, `channel_count`/`linesize` describing the interleaved layout,
	/// `height` 0, format `SLOT_FORMAT_AUDIO_F32` and `data_size` the byte count
	/// actually mixed.
	///
	/// # Returns
	///
	/// The published slot index (always `spec.slot`) for the `frame_ready`
	/// response.
	///
	/// # Errors
	///
	/// Returns a printable message when no valid output pool is attached, when
	/// the acquire times out or shutdown is requested, when the popped slot does
	/// not match `spec.slot` (a protocol violation), when the mixed range needs
	/// more bytes than the slot holds, when `audio_ticket_params` rejects the
	/// geometry, when the mix itself fails, or when the ready ring is full. The
	/// slot stays unpublished on error and is recycled by the main process.
	fn render_audio_ticket_to_slot(&mut self, spec: &AudioTicketSpec) -> Result<u32, String> {
		let pool = match self.output_pool.clone() {
			Some(p) if p.is_valid() => p,
			_ => return Err("no shared-memory pool attached".to_string()),
		};

		let acquired = self
			.acquire_slot(&pool)
			.ok_or_else(|| "no free shm slot (shutdown or timeout)".to_string())?;
		if acquired != spec.slot as u32 {
			return Err(format!(
				"slot assignment mismatch: acquired {acquired}, assigned {}",
				spec.slot
			));
		}
		let slot = acquired;

		let params = self.audio_ticket_params(spec)?;
		let need = eval::audio_samples_byte_len(&params).map_err(|e| e.to_string())?;
		if need > pool.slot_data_bytes() {
			// The slot was acquired but never published; main recycles it on
			// frame_failed (see render_ticket_to_slot).
			return Err(format!(
				"audio range needs {need} bytes, slot holds {}",
				pool.slot_data_bytes()
			));
		}

		// SAFETY: `slot` was acquired above; the block is live shared memory
		// of the attached pool.
		let dst = unsafe {
			slice::from_raw_parts_mut(pool.slot_data(spec.slot as u32), pool.slot_data_bytes())
		};
		eval::render_audio_samples_into(&params, &mut dst[..need]).map_err(|e| e.to_string())?;

		// SAFETY: slot in range of the attached pool.
		unsafe {
			let meta = &mut *pool.meta(spec.slot as u32);
			*meta = FrameSlotMeta::default();
			meta.id = spec.ticket;
			meta.time_num = spec.time_num;
			meta.time_den = spec.time_den;
			// Audio slots reuse the video meta fields: `width` carries the
			// sample rate, `channel_count`/`linesize` describe the interleaved
			// layout, `data_size` is the sample bytes (SLOT_FORMAT_AUDIO_F32).
			meta.width = params.sample_rate;
			meta.height = 0;
			meta.format = SLOT_FORMAT_AUDIO_F32;
			meta.channel_count = params.channel_layout.count_ones().max(1) as i32;
			meta.linesize = meta.channel_count * 4;
			meta.data_size = need as i32;
		}

		let published = unsafe { pool.publish(slot) };
		if !published {
			Err("ready ring full".to_string())
		} else {
			Ok(slot)
		}
	}

	/// Map a wire audio ticket spec to the eval producer's audio params.
	///
	/// `spec` carries the pull range as a start rational plus a duration
	/// rational, the sample rate, the channel layout bitmask, and the montage
	/// clips/effects. The range is built as `[start, start + duration)`, and
	/// `viewer` is the loaded project's copy id (0 when no graph is loaded).
	///
	/// # Errors
	///
	/// Returns a printable message when the geometry is degenerate: a
	/// non-positive `time_den`, `duration_den` or `sample_rate`. These would make
	/// the range rational or the sample count meaningless, so validation happens
	/// here rather than in the render call.
	fn audio_ticket_params(&self, spec: &AudioTicketSpec) -> Result<AudioTicketParams, String> {
		if spec.time_den <= 0 || spec.duration_den <= 0 || spec.sample_rate <= 0 {
			return Err(format!(
				"bad audio geometry: {}x{}, rate {}",
				spec.time_den, spec.duration_den, spec.sample_rate
			));
		}
		let start = Rational::new(spec.time_num, spec.time_den);
		let duration = Rational::new(spec.duration_num, spec.duration_den);
		let montage: Vec<MontageClip> = spec
			.montage
			.iter()
			.map(crate::worker::ipc::montage_clip_from)
			.collect();
		let bus_effects = spec
			.bus_effects
			.iter()
			.map(crate::worker::ipc::montage_audio_effect_from)
			.collect();
		Ok(AudioTicketParams {
			viewer: self.graph.as_ref().map(|g| g.project_copy).unwrap_or(0),
			range: TimeRange::new(start, start + duration),
			sample_rate: spec.sample_rate,
			channel_layout: spec.channel_layout,
			montage,
			bus_effects,
		})
	}

	/// Refresh the process-global pipeline color settings from the loaded
	/// project snapshot (the source of truth for what this worker renders).
	/// Returns true when the settings changed — the caller must drop the
	/// frame cache, whose F32 bytes were produced under the old settings
	/// (M16 S2). The resync keeps the global current via `load_graph`; this
	/// covers tickets in flight before that IPC lands, and mirrors
	/// [`handle_load_graph`]'s adopt step.
	fn sync_pipeline_color_from_graph(&mut self) -> bool {
		let Some(graph) = &self.graph else {
			return false;
		};
		let Some(project) = &graph.project else {
			return false;
		};
		let (working, output) = {
			let guard = project.lock().unwrap_or_else(|e| e.into_inner());
			(guard.working_color_space(), guard.output_color_spec())
		};
		if color::pipeline_working_space() == working && color::pipeline_output_spec() == output {
			return false;
		}
		color::set_pipeline_color_settings(working, output);
		true
	}

	/// Render one ticket's pixels into the slot's data block and fill in the
	/// slot meta.
	///
	/// `spec.slot` must already have been acquired by `render_ticket_to_slot`;
	/// this function only writes the slot's data and meta, it never publishes.
	/// The picture is produced in F32 RGBA and, for `SLOT_FORMAT_BGRA8`
	/// requests, run through the output color node and quantized at the end of
	/// the pipe. Pipeline color settings are resynced from the loaded project
	/// first, and a change clears the F32 frame cache because its bytes were
	/// rendered under the old settings; otherwise a cache hit is copied instead
	/// of re-rendering. The meta written is: ticket id, time rational, width and
	/// height, the requested slot format, `max(channels, 4)` channel count,
	/// row size in bytes and total data size.
	///
	/// # Errors
	///
	/// Returns a printable message (surfaced as `frame_failed`) when
	/// `spec.width`/`spec.height` is not positive, when the slot's data block is
	/// too small for `height` rows of the requested format, or when the
	/// underlying graph/montage/footage render fails. The slot is left
	/// unpublished in every error case; the main process recycles it.
	fn render_spec_pixels(
		&mut self,
		spec: &BatchTicketSpec,
		pool: &FrameSlotPool,
	) -> Result<(), String> {
		// Derive the pipeline colors from the loaded project on every render:
		// eval's decode linearization and the output node below read the
		// process global, which the resync (load_graph) keeps current — but a
		// ticket in flight before that IPC lands must not render under stale
		// colors. A change invalidates the F32 frame cache: cached bytes were
		// produced under the previous settings (M16 S2).
		if self.sync_pipeline_color_from_graph() {
			self.frame_cache.clear();
		}
		let w = spec.width;
		let h = spec.height;
		if w <= 0 || h <= 0 {
			return Err(format!("bad render size {w}x{h}"));
		}
		let time = Rational::new(spec.time_num, spec.time_den);
		let params = self.ticket_params(spec, time);

		let bgra8 = spec.format == SLOT_FORMAT_BGRA8;
		let (dst_bpp, dst_linesize) = if bgra8 { (4, w * 4) } else { (16, w * 16) };
		let dst_need = (h as usize) * (dst_linesize as usize);
		if dst_need > pool.slot_data_bytes() {
			return Err(format!(
				"frame {}x{} needs {dst_need} bytes, slot holds {}",
				w,
				h,
				pool.slot_data_bytes()
			));
		}
		let _ = dst_bpp;

		// SAFETY: `spec.slot` was acquired by render_ticket_to_slot; the
		// block is live shared memory of the attached pool.
		let dst = unsafe {
			slice::from_raw_parts_mut(pool.slot_data(spec.slot as u32), pool.slot_data_bytes())
		};

		// M16 S2 frame cache: memoized F32 pipeline bytes keyed by the
		// render-deterministic spec subset (ticket/slot/format/channels are
		// delivery, not picture — an F32 and a BGRA8 request for the same
		// frame share one entry). A hit is a copy (plus the end-of-pipe
		// convert for BGRA8 slots); a miss renders as before and memoizes
		// the F32 bytes. See [`crate::worker::framecache`].
		let f32_need = (w as usize) * (h as usize) * 16;
		let key = crate::worker::framecache::spec_cache_key(spec);
		let mut cached = self.frame_cache.get(&key).map(|b| b.to_vec());
		if let Some(c) = &cached {
			if c.len() < f32_need {
				// Defensive: an entry smaller than this geometry is not for
				// this frame (keys are geometry-signed); re-render.
				cached = None;
			}
		}

		if !bgra8 {
			// F32 RGBA: render straight into the slot (no staging copy).
			match &cached {
				Some(c) => dst[..f32_need].copy_from_slice(&c[..f32_need]),
				None => {
					render_f32_into(
						spec,
						&params,
						&self.graph,
						time,
						(w, h),
						&mut dst[..dst_need],
					)?;
					self.frame_cache.insert(key, dst[..f32_need].to_vec());
				}
			}
		} else {
			// BGRA8: the F32 pipeline frame comes from the cache or the
			// session scratch, then converts into the slot (the end-of-pipe
			// format convert is not an extra frame copy, design §3.1).
			// Before quantization the output node runs: working space
			// (ACEScg) → the project's output colorspace, so the 8-bit
			// pixels carry gamma-encoded display values, not linear light.
			match cached.as_mut() {
				Some(c) => {
					apply_output_node(c, (w * h) as usize);
					convert_f32_rgba_to_bgra8(&c[..f32_need], &mut dst[..dst_need]);
				}
				None => {
					if self.f32_scratch.len() < f32_need {
						self.f32_scratch.resize(f32_need, 0);
					}
					render_f32_into(
						spec,
						&params,
						&self.graph,
						time,
						(w, h),
						&mut self.f32_scratch[..f32_need],
					)?;
					self.frame_cache
						.insert(key, self.f32_scratch[..f32_need].to_vec());
					apply_output_node(&mut self.f32_scratch, (w * h) as usize);
					convert_f32_rgba_to_bgra8(&self.f32_scratch[..f32_need], &mut dst[..dst_need]);
				}
			}
		}

		// Slot meta (fresh each publish).
		// SAFETY: slot in range of the attached pool.
		unsafe {
			let meta = &mut *pool.meta(spec.slot as u32);
			*meta = FrameSlotMeta::default();
			meta.id = spec.ticket;
			meta.time_num = spec.time_num;
			meta.time_den = spec.time_den;
			meta.width = w;
			meta.height = h;
			meta.format = spec.format;
			meta.channel_count = spec.channels.max(4);
			meta.linesize = dst_linesize;
			meta.data_size = dst_need as i32;
		}
		Ok(())
	}

	/// Map a wire batch ticket spec to the eval producer's video ticket params.
	///
	/// `spec` carries the ticket's delivery fields plus the render
	/// description; `time` is the already-built rational from
	/// `spec.time_num`/`spec.time_den` (the caller needs it separately, so it is
	/// not rebuilt here). The viewer identity is `spec.viewer_node` when
	/// non-zero, otherwise the loaded project's copy id (0 when no graph has
	/// been loaded) — the montage/footage paths do not use it. A non-empty
	/// `spec.footage_file` becomes `Some((file, stream))`; montage clips and
	/// adjustments are converted from their wire form.
	///
	/// The returned params force the full ticket size and `PixelFormat::F32`:
	/// the pipeline always renders F32 and any BGRA8 conversion happens at the
	/// end of the pipe, and the frame cache is left disabled because the worker
	/// keeps its own memo (`frame_cache`).
	fn ticket_params(&self, spec: &BatchTicketSpec, time: Rational) -> VideoTicketParams {
		let footage = if spec.footage_file.is_empty() {
			None
		} else {
			Some((spec.footage_file.clone(), spec.footage_stream))
		};
		let montage: Vec<MontageClip> = spec
			.montage
			.iter()
			.map(crate::worker::ipc::montage_clip_from)
			.collect();
		let adjustments: Vec<AdjustmentSpan> = spec
			.adjustments
			.iter()
			.map(quercus_render::ipc::adjustment_from_wire)
			.collect();
		VideoTicketParams {
			viewer: if spec.viewer_node != 0 {
				spec.viewer_node
			} else {
				self.graph.as_ref().map(|g| g.project_copy).unwrap_or(0)
			},
			project: spec.project_key.clone(),
			time,
			force_size: Some((spec.width, spec.height)),
			force_format: Some(PixelFormat::F32),
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage,
			montage,
			adjustments,
		}
	}
}

/// Render the F32 RGBA pipeline frame for `spec` into `dst`
/// (`(w*h*16)` bytes): graph-mode viewer frame, generated transparent
/// black, footage decode, or montage composite — through
/// [`quercus_render::eval`].
fn render_f32_into(
	spec: &BatchTicketSpec,
	params: &VideoTicketParams,
	graph: &Option<LoadedGraph>,
	time: Rational,
	size: (i32, i32),
	dst: &mut [u8],
) -> Result<(), String> {
	let (w, h) = size;
	let stride = w * 16;
	// M16 S1 graph mode: render the ticket's viewer node from the loaded
	// snapshot — but only when the snapshot belongs to the ticket's own
	// project. The ticket carries its owning project's uuid; a stale graph
	// from a different project (fresh projects reuse small identity
	// numbers, so a foreign viewer identity can resolve successfully and
	// silently render the wrong picture) must never answer it — that ticket
	// falls through to the montage path instead, and is NOT logged (the
	// mismatch is the normal suite-order case, not an error). A missing
	// graph / viewer is logged once and also falls back.
	if spec.viewer_node != 0 {
		let project_matches = graph
			.as_ref()
			.and_then(|g| g.project_uuid.as_deref())
			.map(|uuid| uuid == spec.project_key.as_str())
			.unwrap_or(false);
		if project_matches {
			if let Some(project) = graph.as_ref().and_then(|g| g.project.as_ref()) {
				// The ticket's viewer is an identity in the *saved* project;
				// translate through the load map first, then fall back to the
				// raw packed id (identity-only snapshots, foreign files).
				let viewer_id = graph
					.as_ref()
					.and_then(|g| g.id_map.get(&spec.viewer_node).copied())
					.or_else(|| id::NodeId::from_identity(spec.viewer_node));
				match viewer_id {
					Some(viewer_id) => {
						let rendered = eval::render_graph_frame(
							project,
							viewer_id,
							time,
							(w, h),
							PixelFormat::F32,
						);
						// M2: the graph renders all-GPU in-process; the worker's
						// wire format is a CPU shm slot, so this is the explicit
						// readback boundary of the process backend.
						let frame = match rendered {
							Ok(texture::Texture::Cpu(frame)) => Some(frame),
							Ok(texture @ texture::Texture::Gpu { .. }) => {
								match texture.to_frame() {
									Ok(frame) => Some(frame),
									Err(e) => {
										warn_graph_fallback(spec.viewer_node, &e.to_string());
										None
									}
								}
							}
							Ok(texture::Texture::Planar(_)) => {
								// The footage path resolves imported planar
								// frames before returning; a planar texture
								// here is a bug, not a frame.
								warn_graph_fallback(spec.viewer_node, "unresolved planar texture");
								None
							}
							Err(e) => {
								warn_graph_fallback(spec.viewer_node, &e.to_string());
								None
							}
						};
						if let Some(frame) = frame {
							let src_stride = frame.linesize_bytes();
							let row_bytes = (w as usize) * 16;
							if frame.data.len() < src_stride * (h as usize)
								|| dst.len() < row_bytes * (h as usize)
							{
								return Err("graph frame geometry mismatch".to_string());
							}
							for y in 0..h as usize {
								dst[y * row_bytes..(y + 1) * row_bytes].copy_from_slice(
									&frame.data[y * src_stride..y * src_stride + row_bytes],
								);
							}
							return Ok(());
						}
					}
					None => warn_graph_fallback(spec.viewer_node, "viewer node not in graph"),
				}
			} else {
				warn_graph_fallback(spec.viewer_node, "no loaded graph");
			}
		}
	}
	if !params.montage.is_empty() {
		// Prefer the GPU montage compositor: decode/import, GPU scale and
		// composite stay on the GPU; only the worker's shared-memory wire
		// format requires the final readback into `dst`.
		let rendered = eval::render_montage_frame(time, params, (w, h), PixelFormat::F32)
			.map_err(|e| format!("montage render: {e}"))?;
		let frame = match rendered {
			texture::Texture::Cpu(frame) => frame,
			texture @ texture::Texture::Gpu { .. } => texture
				.to_frame()
				.map_err(|e| format!("montage readback: {e}"))?,
			texture::Texture::Planar(_) => {
				return Err("unresolved planar montage texture".to_string());
			}
		};
		let src_stride = frame.linesize_bytes();
		let row_bytes = (w as usize) * 16;
		if frame.data.len() < src_stride * (h as usize)
			|| dst.len() < row_bytes * (h as usize)
			|| stride < row_bytes as i32
		{
			return Err("montage frame geometry mismatch".to_string());
		}
		for y in 0..h as usize {
			dst[y * stride as usize..y * stride as usize + row_bytes]
				.copy_from_slice(&frame.data[y * src_stride..y * src_stride + row_bytes]);
		}
		return Ok(());
	}
	if !spec.footage_file.is_empty() {
		let decoded = eval::render_footage_frame(
			&spec.footage_file,
			spec.footage_stream,
			time,
			(w, h),
			PixelFormat::F32,
		)
		.map_err(|e| format!("footage decode: {e}"))?;
		// M2: decode stays CPU for now (M5 makes it GPU); an imported GPU
		// texture would still have to cross into the shm slot here.
		let frame = match &decoded {
			texture::Texture::Cpu(frame) => frame.clone(),
			gpu @ texture::Texture::Gpu { .. } => gpu
				.to_frame()
				.map_err(|e| format!("decode readback: {e}"))?,
			// Resolved by the footage path; a planar frame cannot cross
			// into the shm slot without a readback.
			texture::Texture::Planar(_) => {
				return Err("unresolved planar decode texture".to_string());
			}
		};
		let src_stride = frame.linesize_bytes() as usize;
		let row_bytes = (w as usize) * 16;
		if frame.data.len() < src_stride * (h as usize) || dst.len() < row_bytes * (h as usize) {
			return Err("decoded frame geometry mismatch".to_string());
		}
		for y in 0..h as usize {
			dst[y * row_bytes..(y + 1) * row_bytes]
				.copy_from_slice(&frame.data[y * src_stride..y * src_stride + row_bytes]);
		}
		return Ok(());
	}
	// Generated frame: transparent black.
	dst[..(h as usize) * (stride as usize)].fill(0);
	Ok(())
}

/// Log (once per process) why a graph-mode ticket fell back to the
/// montage path — a missing snapshot, an absent viewer node, or a graph
/// render error. AtomicBool keeps a GPU-less or graph-less host from
/// spamming the worker log on every frame.
fn warn_graph_fallback(viewer: u64, why: &str) {
	static WARNED: AtomicBool = AtomicBool::new(false);
	if !WARNED.swap(true, Ordering::Relaxed) {
		log_error(&format!(
			"graph-mode ticket viewer {viewer}: {why}; falling back to montage"
		));
	}
}

/// Convert F32 RGBA (`src`, 16 bytes/px) to 8-bit BGRA (`dst`, 4
/// bytes/px) with clamping — the worker-side end-of-pipe convert for
/// BGRA8 preview slots.
fn convert_f32_rgba_to_bgra8(src: &[u8], dst: &mut [u8]) {
	let to_u8 = |v: f32| -> u8 { (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8 };
	let pixels = dst.len() / 4;
	for i in 0..pixels {
		let s = &src[i * 16..i * 16 + 16];
		let r = f32::from_le_bytes(s[0..4].try_into().unwrap());
		let g = f32::from_le_bytes(s[4..8].try_into().unwrap());
		let b = f32::from_le_bytes(s[8..12].try_into().unwrap());
		let a = f32::from_le_bytes(s[12..16].try_into().unwrap());
		let d = &mut dst[i * 4..i * 4 + 4];
		d[0] = to_u8(b);
		d[1] = to_u8(g);
		d[2] = to_u8(r);
		d[3] = to_u8(a);
	}
}

/// The output node for the BGRA8 delivery path: convert the first
/// `pixels` pixels of an F32 RGBA byte buffer from the pipeline working
/// space to the project's output colorspace (in place), so the 8-bit
/// quantization encodes display-referred values instead of linear light.
/// A no-op in the legacy sRGB working space (content already is
/// display-referred sRGB).
fn apply_output_node(bytes: &mut [u8], pixels: usize) {
	if color::pipeline_working_space() == colormath::WorkingColorSpace::SrgbLegacy {
		return;
	}
	let spec = color::pipeline_output_spec();
	colormath::acescg_to_output_bytes(bytes, pixels, spec);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

/// Full render-worker main, transport-agnostic in the backend name.
///
/// Mirrors `quercusengine_worker_main()` in worker.cpp: create the session
/// (which initializes the render backend), load the runtime config, write
/// the startup handshake, then serve the NDJSON control loop on
/// stdin/stdout until a `shutdown` message or EOF. Returns the process
/// exit code.
pub fn worker_main(backend: &str) -> i32 {
	// 1. Session creation initializes the render backend through the
	//    quercusrender crate's direct Rust API
	//    (quercusengine_worker_session_create()). The M15 "cpu" backend is
	//    headless: no renderer, CPU evaluation + decode only. M16 S1: a
	//    failed GPU init for any other backend (e.g. "auto" on a GPU-less
	//    host) is tolerated — the session continues headless.
	let mut session = match WorkerSession::create(backend) {
		Ok(s) => s,
		Err(msg) => {
			log_error(&msg);
			return 1;
		}
	};
	if !session.has_renderer() && is_no_backend(backend) {
		// Mirrors quercusengine_worker_main(): an explicit no-renderer request
		// (""/"none") leaves nothing to evaluate, so the worker exits 1.
		// Any other backend failure already logged headless continuation.
		log_error("no renderer initialized");
		return 1;
	}

	// 2. Runtime services (config load, plugin executor install).
	if !session.initialize_runtime() {
		return 1;
	}

	// 3. Startup handshake before the loop (mirrors worker.cpp main).
	let handshake = session.startup_handshake();
	let stdout = io::stdout();
	let mut out = io::BufWriter::new(stdout.lock());
	if let Err(e) = write_message(&mut out, &handshake) {
		log_error(&format!("failed to write startup handshake: {e}"));
		return 1;
	}
	if let Err(e) = out.flush() {
		log_error(&format!("failed to flush startup handshake: {e}"));
		return 1;
	}

	// 4. NDJSON control loop until a shutdown message or EOF.
	let stdin = io::stdin();
	let mut reader = stdin.lock();
	let mut line = String::new();
	let mut exit_code = 0;
	while !session.shutdown_requested() {
		line.clear();
		match reader.read_line(&mut line) {
			Ok(0) => break, // EOF: the parent closed the control pipe.
			Ok(_) => {}
			Err(e) => {
				log_error(&format!("failed to read control line: {e}"));
				break;
			}
		}
		if line.trim().is_empty() {
			// Blank lines are skipped silently (read_message() semantics).
			continue;
		}
		// Protocol v2: render_batch streams its responses (one
		// batch_accepted + one frame_ready/frame_failed per ticket).
		let typ = serde_json::from_str::<Value>(&line)
			.ok()
			.and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_string));
		match typ.as_deref() {
			Some(TYPE_RENDER_BATCH) => {
				if let Err(e) = session.handle_render_batch_stream(&line, &mut out) {
					log_error(&format!("failed to serve render_batch: {e}"));
					exit_code = 1;
					break;
				}
				// Plugin progress events buffered during the batch go out now.
				flush_worker_progress(&mut out);
				continue;
			}
			// M15 S3: render_audio_batch streams the same way (audio range
			// pulls into shm slots, interleaved f32).
			Some(TYPE_RENDER_AUDIO_BATCH) => {
				if let Err(e) = session.handle_render_audio_batch_stream(&line, &mut out) {
					log_error(&format!("failed to serve render_audio_batch: {e}"));
					exit_code = 1;
					break;
				}
				flush_worker_progress(&mut out);
				continue;
			}
			_ => {}
		}
		if let Some(response) = session.handle_line(&line) {
			if let Err(e) = write_message(&mut out, &response) {
				log_error(&format!("failed to write response: {e}"));
				exit_code = 1;
				break;
			}
			if let Err(e) = out.flush() {
				log_error(&format!("failed to flush response: {e}"));
				exit_code = 1;
				break;
			}
		}
		// Progress events buffered while serving the control message.
		flush_worker_progress(&mut out);
	}
	exit_code
}

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// C ABI used by the thin `quercus-worker` launcher through the Rust SDK
// ---------------------------------------------------------------------------

/// Run the ticket worker loop for the given render backend and return the
/// process exit code.
///
/// This is the C entry point the thin `quercus-worker` launcher calls. The
/// backend bytes are decoded to a string (null or empty becomes `"opengl"`,
/// the same default as the launcher), then `worker_main` runs the NDJSON
/// control loop on the current thread — it blocks until the parent closes the
/// pipe or sends a shutdown message. Panics are caught so they cannot unwind
/// across the C ABI.
///
/// # Arguments
///
/// * `backend` - Backend name bytes: `"opengl"`, `"vulkan"`, `"cpu"`, `"none"`
///   etc. May be null, in which case `"opengl"` is used; not NUL-terminated,
///   not retained.
/// * `backend_len` - Length of `backend` in bytes; ignored when it is null.
///
/// # Returns
///
/// `worker_main`'s exit code, or `1` when it panicked.
///
/// # Safety
///
/// `backend` must be null or valid for reading `backend_len` bytes for the
/// whole call. Invalid UTF-8 is replaced (lossy conversion) rather than
/// rejected, so the backend name may silently change; pass ASCII names.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_worker_run(backend: *const u8, backend_len: usize) -> i32 {
	let backend = if backend.is_null() {
		String::new()
	} else {
		String::from_utf8_lossy(unsafe { slice::from_raw_parts(backend, backend_len) }).into_owned()
	};
	let backend = if backend.is_empty() {
		"opengl".to_string()
	} else {
		backend
	};
	panic::catch_unwind(|| worker_main(&backend)).unwrap_or(1)
}

/// Run the single OpenFX host loop in this process and return its exit code.
///
/// This is the C entry point the thin `quercus-worker` launcher calls when the
/// worker is used as an OFX host: it decodes `args` into argv (the first entry
/// is the program name, which the crash-hook test flags parse around), calls
/// `ofx_host::ofx_host_main`, and catches panics so they cannot unwind across
/// the C ABI.
///
/// # Arguments
///
/// * `args` - Newline-separated argv bytes, argv[0] included. May be null, in
///   which case it is treated as an empty argument list; the buffer does not
///   have to be NUL-terminated and is neither retained nor modified.
/// * `args_len` - Length of `args` in bytes; ignored when `args` is null.
///
/// # Returns
///
/// The exit code produced by the host loop (`0` for success), or `1` when the
/// loop panicked.
///
/// # Safety
///
/// `args` must be null or valid for reading `args_len` bytes for the whole
/// call. The bytes are interpreted as UTF-8 on a lossy basis, so invalid
/// sequences are replaced rather than rejected.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_worker_ofx_host(args: *const u8, args_len: usize) -> i32 {
	let text = if args.is_null() {
		String::new()
	} else {
		String::from_utf8_lossy(unsafe { slice::from_raw_parts(args, args_len) }).into_owned()
	};
	let args: Vec<String> = if text.is_empty() {
		Vec::new()
	} else {
		text.split('\n').map(str::to_owned).collect()
	};
	panic::catch_unwind(|| ofx_host::ofx_host_main(&args)).unwrap_or(1)
}
