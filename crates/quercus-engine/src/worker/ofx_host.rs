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

//! `quercus-worker --ofx-host`: the single OpenFX host process (M3, design
//! §3.2).
//!
//! The host loads every OFX plugin once (the worker binary's normal plugin
//! runtime) and then serves `ofx_job` messages from the main process over
//! NDJSON, with frames moving through the input/output
//! [`FrameSlotPool`]s announced by the handshake. Each job is resolved to
//! a host-local instance by the plugin **identifier** (the cross-process
//! stable key) and rendered through the same in-process executor the
//! workers used to install ([`quercus_ofx_plugin::node_factory::install_render_executor`]).
//!
//! Progress is flushed to stdout immediately (the main process's reader
//! forwards it to the plugin-progress dialog), and `plugin_cancel` sets
//! the same sticky flag protocol the worker used: the next `progressStart`
//! resets it and every `progressUpdate` after a cancel answers false, so
//! the plugin aborts at its next progress call.
//!
//! The host is deliberately single-threaded and synchronous, matching the
//! worker model; crash isolation comes from the parent respawning it.

use quercus_ofx_plugin::host;
use quercus_ofx_plugin::node_factory;
use quercus_ofx_plugin::progress;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process;
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use quercus_core::PixelFormat;
use quercus_core::frame::VideoParamsPod;
use quercus_core::texture::{Frame, Texture};
use quercus_render::eval::{self, JobSpec, PluginJobRequest};
use quercus_render::ipc::{
	FrameSlotPool, HandshakeMsg, OfxJobMsg, OfxResultMsg, PluginProgressMsg, SharedMemoryRegion,
	ShmMode, TYPE_HANDSHAKE, TYPE_OFX_JOB, TYPE_PLUGIN_CANCEL, TYPE_SHUTDOWN, error_message,
	write_message,
};

/// Sticky plugin cancel (protocol parity with `worker.rs`): set by
/// `plugin_cancel`, reset by the next `progressStart`.
static OFX_CANCEL: AtomicBool = AtomicBool::new(false);

/// stdout is shared by progress events (emitted from inside the plugin
/// render) and control responses.
static OUT_LOCK: Mutex<()> = Mutex::new(());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write one NDJSON line to stdout, flushed (progress must stream while
/// the job renders). No-op under unit tests (they exercise the reporter
/// return values, not the pipe).
fn emit(value: &serde_json::Value) {
	if cfg!(test) {
		return;
	}
	let _guard = lock(&OUT_LOCK);
	let stdout = std::io::stdout();
	let mut out = stdout.lock();
	let _ = write_message(&mut out, value);
	let _ = out.flush();
}

/// The host's progress reporter: emits `plugin_progress` immediately and
/// reports cancellation to the plugin.
struct HostProgressReporter {
	label: String,
	message: String,
}

impl progress::UiProgressReporter for HostProgressReporter {
	fn update(&mut self, progress: f64) -> bool {
		emit(
			&PluginProgressMsg {
				label: self.label.clone(),
				message: self.message.clone(),
				fraction: progress.clamp(0.0, 1.0),
			}
			.to_json(),
		);
		!OFX_CANCEL.load(Ordering::Relaxed)
	}

	fn end(&mut self) {
		emit(
			&PluginProgressMsg {
				label: self.label.clone(),
				message: self.message.clone(),
				fraction: 1.0,
			}
			.to_json(),
		);
	}
}

/// Build one progress reporter for `progressStart`.
///
/// `label` / `message` name the plugin and operation in the emitted
/// `plugin_progress` events. The reporter clears the sticky cancel (the protocol's
/// "a fresh render starts uncancelled"), emits the fraction-0 start event, and
/// returns a reporter that emits each fraction clamped to `0..=1` and answers
/// `false` once a cancel arrived. Installed as the reporter factory by
/// `install_progress_factory`; tests call it directly.
fn host_progress_reporter(label: &str, message: &str) -> Box<dyn progress::UiProgressReporter> {
	OFX_CANCEL.store(false, Ordering::Relaxed);
	emit(
		&PluginProgressMsg {
			label: label.to_string(),
			message: message.to_string(),
			fraction: 0.0,
		}
		.to_json(),
	);
	Box::new(HostProgressReporter {
		label: label.to_string(),
		message: message.to_string(),
	})
}

/// Install the reporter factory (`progressStart` → [`host_progress_reporter`]).
fn install_progress_factory() {
	progress::set_reporter_factory(Some(Arc::new(host_progress_reporter)));
}

/// Start the stdin reader thread and return the channel of control lines.
///
/// Every non-empty line is forwarded to the main loop, except `plugin_cancel`,
/// which sets the sticky `OFX_CANCEL` flag inline so a cancel is observed while a
/// plugin render is in flight. Invalid JSON is still forwarded (the main loop
/// rejects it). EOF or a send error ends the thread and drops the sender, which
/// closes the channel and therefore ends the main loop.
///
/// Panics (in the spawning thread) when the reader thread cannot be created.
fn spawn_stdin_reader() -> mpsc::Receiver<String> {
	let (tx, rx) = mpsc::channel();
	thread::Builder::new()
		.name("quercus-ofx-host-stdin".into())
		.spawn(move || {
			let stdin = std::io::stdin();
			let mut reader = BufReader::new(stdin.lock());
			let mut line = String::new();
			loop {
				line.clear();
				match reader.read_line(&mut line) {
					Ok(0) | Err(_) => break,
					Ok(_) => {}
				}
				if line.trim().is_empty() {
					continue;
				}
				if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
					if value.get("type").and_then(|t| t.as_str()) == Some(TYPE_PLUGIN_CANCEL) {
						OFX_CANCEL.store(true, Ordering::Relaxed);
						continue;
					}
				}
				if tx.send(line.clone()).is_err() {
					break;
				}
			}
		})
		.expect("spawn OFX host stdin reader");
	rx
}

/// The attached pools. The `SharedMemoryRegion`s must outlive the pools
/// (the pool views point into the mappings).
struct HostPools {
	_input_region: SharedMemoryRegion,
	input: FrameSlotPool,
	_output_region: SharedMemoryRegion,
	output: FrameSlotPool,
}

/// Attach the pools announced by a `handshake` (the worker's
/// `handle_handshake`, minus the render session).
///
/// `msg` is the handshake JSON: `shm_key` / `output_slots` / `slot_data_bytes` for
/// the output pool and `input_shm_key` / `input_slots` / `input_slot_data_bytes`
/// for the input pool. Each region is attached at the size the pool needs and
/// validated as a frame slot pool; the regions are kept in the returned
/// [`HostPools`] so the pool views stay mapped for the host's lifetime.
///
/// # Errors
///
/// A message naming the missing geometry, the failing attach, or the region that
/// does not contain a slot pool.
fn attach_pools(msg: &serde_json::Value) -> Result<HostPools, String> {
	let hs: HandshakeMsg =
		serde_json::from_value(msg.clone()).map_err(|e| format!("invalid handshake: {e}"))?;
	if hs.shm_key.is_empty() || hs.output_slots <= 0 || hs.slot_data_bytes <= 0 {
		return Err("handshake missing output shared-memory geometry".to_string());
	}
	let output_bytes =
		FrameSlotPool::bytes_needed(hs.output_slots as u32, hs.slot_data_bytes as usize);
	let mut output_region = SharedMemoryRegion::new();
	if !output_region.open(&hs.shm_key, output_bytes, ShmMode::Attach) {
		return Err(format!(
			"failed to attach output shared memory: {}",
			output_region.error()
		));
	}
	// SAFETY: the mapping is live and sized for the pool.
	let output = unsafe { FrameSlotPool::attach(output_region.data()) };
	if !output.is_valid() {
		return Err("output shared memory does not contain a frame slot pool".to_string());
	}

	if hs.input_shm_key.is_empty() || hs.input_slots <= 0 || hs.input_slot_data_bytes <= 0 {
		return Err("handshake missing input shared-memory geometry".to_string());
	}
	let input_bytes =
		FrameSlotPool::bytes_needed(hs.input_slots as u32, hs.input_slot_data_bytes as usize);
	let mut input_region = SharedMemoryRegion::new();
	if !input_region.open(&hs.input_shm_key, input_bytes, ShmMode::Attach) {
		return Err(format!(
			"failed to attach input shared memory: {}",
			input_region.error()
		));
	}
	// SAFETY: the mapping is live and sized for the pool.
	let input = unsafe { FrameSlotPool::attach(input_region.data()) };
	if !input.is_valid() {
		return Err("input shared memory does not contain a frame slot pool".to_string());
	}

	Ok(HostPools {
		_input_region: input_region,
		input,
		_output_region: output_region,
		output,
	})
}

/// Consume one input frame from the slot pool (the producer is the main process).
///
/// `pool` is the input pool and `slot` the slot index the job announced. The slot
/// must be at the head of the ready ring: a mismatch releases the popped slot and
/// fails. The metadata is validated (positive width/height, non-negative data
/// size), the pixels are copied out as F32 RGBA, the shm slot is released and a
/// [`Frame`] carrying those params and bytes is returned.
///
/// # Errors
///
/// `"input slot missing"` when nothing is ready, an error naming both slot indices
/// on a mismatch, and an invalid-metadata error for a bad geometry; the consumed
/// slot is released on the error paths.
fn read_input_frame(pool: &FrameSlotPool, slot: u32) -> Result<Frame, String> {
	let mut consumed = 0u32;
	// SAFETY: consumer side of the SPSC protocol, single-threaded host.
	if !unsafe { pool.consume(&mut consumed) } {
		return Err("input slot missing".to_string());
	}
	if consumed != slot {
		unsafe {
			pool.release(consumed);
		}
		return Err(format!(
			"input slot mismatch: expected {slot}, got {consumed}"
		));
	}
	// SAFETY: `slot` was just consumed; the meta POD is initialized by the
	// producer before publish.
	let meta = unsafe { &*pool.meta_const(slot) };
	if meta.width <= 0 || meta.height <= 0 || meta.data_size < 0 {
		unsafe {
			pool.release(slot);
		}
		return Err("input frame has invalid metadata".to_string());
	}
	let len = (meta.data_size as usize).min(pool.slot_data_bytes());
	// SAFETY: `len` is within the slot block.
	let data = unsafe { slice::from_raw_parts(pool.slot_data_const(slot), len).to_vec() };
	unsafe {
		pool.release(slot);
	}
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = meta.width;
	pod.height = meta.height;
	pod.format = PixelFormat::F32 as i32;
	frame.set_video_params(pod);
	frame.data = data;
	Ok(frame)
}

/// Publish one output frame to the slot pool (the consumer is the main process).
///
/// `pool` is the output pool and `frame` the F32 RGBA frame to copy. A slot is
/// acquired, the bytes are copied in when they fit the slot capacity, the metadata
/// is written (F32, 4 channels, line size, data size and the frame timestamp) and
/// the slot is published. Returns the published slot index.
///
/// # Errors
///
/// `"output pool is full"` when no slot can be acquired, an error naming both
/// sizes when the frame is larger than a slot, and `"output publish failed"` when
/// the ring rejects the publish; the acquired slot is released on every one of
/// those paths.
fn write_output_frame(pool: &FrameSlotPool, frame: &Frame) -> Result<i32, String> {
	let mut slot = 0u32;
	// SAFETY: producer side of the SPSC protocol, single-threaded host.
	if !unsafe { pool.acquire(&mut slot) } {
		return Err("output pool is full".to_string());
	}
	let bytes = frame.data.len();
	if bytes > pool.slot_data_bytes() {
		unsafe {
			pool.release(slot);
		}
		return Err(format!(
			"output frame is {bytes} bytes, larger than the output slot ({})",
			pool.slot_data_bytes()
		));
	}
	// SAFETY: `slot` was just acquired; fill then publish.
	unsafe {
		ptr::copy_nonoverlapping(frame.data.as_ptr(), pool.slot_data(slot), bytes);
		let meta = &mut *pool.meta(slot);
		*meta = Default::default();
		meta.width = frame.width;
		meta.height = frame.height;
		meta.format = PixelFormat::F32 as i32;
		meta.channel_count = 4;
		meta.linesize = frame.linesize_bytes() as i32;
		meta.data_size = bytes as i32;
		meta.time_num = frame.timestamp.numerator();
		meta.time_den = frame.timestamp.denominator().max(1);
		if !pool.publish(slot) {
			pool.release(slot);
			return Err("output publish failed".to_string());
		}
	}
	Ok(slot as i32)
}

/// Render one `ofx_job` and return its `ofx_result` response.
///
/// `msg` is the parsed JSON job (`OfxJobMsg`: job id, plugin type id, time,
/// named input slots, optional src slot, effect input id and parameter values);
/// `pools` is the handshake-attached input/output slot pair. Every named input and
/// the optional source are consumed from the input pool; a missing source falls
/// back to the declared effect input, then the first input, then a dummy texture
/// (mirroring the evaluator). The plugin instance comes from the installed
/// factory, the render runs through the installed executor, and the resulting
/// texture is read back and published to the output pool.
///
/// Returns an `OfxResultMsg` JSON: the output slot with an empty error on success,
/// or `slot: -1` plus a message for an invalid job, a bad/missing input slot, a
/// missing factory or executor, an unknown plugin, a render/readback failure or a
/// full output pool.
fn handle_job(msg: serde_json::Value, pools: &HostPools) -> serde_json::Value {
	let job: OfxJobMsg = match serde_json::from_value(msg) {
		Ok(job) => job,
		Err(err) => return error_message(&format!("invalid ofx_job: {err}"), None),
	};
	let fail = |message: String| {
		OfxResultMsg {
			job: job.job,
			slot: -1,
			error: message,
		}
		.to_json()
	};

	let mut inputs = Vec::with_capacity(job.inputs.len());
	for input in &job.inputs {
		match read_input_frame(&pools.input, input.slot) {
			Ok(frame) => inputs.push((input.name.clone(), Texture::wrap_frame(frame))),
			Err(err) => return fail(err),
		}
	}
	let src = match job.src_slot {
		Some(slot) => match read_input_frame(&pools.input, slot) {
			Ok(frame) => Texture::wrap_frame(frame),
			Err(err) => return fail(err),
		},
		None => {
			// No explicit source: mirror the evaluator's fallback (the
			// declared effect input, else the first clip, else dummy).
			let named = inputs
				.iter()
				.find(|(name, _)| name == &job.effect_input_id)
				.or_else(|| inputs.first())
				.map(|(_, texture)| texture.clone());
			named.unwrap_or_else(Texture::dummy)
		}
	};

	let Some(factory) = eval::plugin_instance_factory() else {
		return fail("no plugin instance factory installed in the OFX host".to_string());
	};
	let Some(instance) = factory(&job.type_id) else {
		return fail(format!(
			"unknown or unavailable OFX plugin: {}",
			job.type_id
		));
	};
	let Some(executor) = eval::plugin_executor() else {
		return fail("no plugin executor installed in the OFX host".to_string());
	};
	let spec = JobSpec::Plugin {
		instance,
		type_id: job.type_id.clone(),
		time: job.time,
		effect_input_id: if job.effect_input_id.is_empty() {
			None
		} else {
			Some(job.effect_input_id.clone())
		},
		inputs,
		values: job
			.values
			.iter()
			.map(|param| (param.input.clone(), param.value.to_node_value()))
			.collect(),
		animations: job
			.animations
			.iter()
			.map(crate::worker::ipc::WireEffectAnimation::to_track)
			.collect(),
	};
	match executor(&PluginJobRequest { spec: &spec, src }) {
		Ok(texture) => match texture.to_frame() {
			Ok(frame) => match write_output_frame(&pools.output, &frame) {
				Ok(slot) => OfxResultMsg {
					job: job.job,
					slot,
					error: String::new(),
				}
				.to_json(),
				Err(err) => fail(err),
			},
			Err(err) => fail(format!("plugin output readback failed: {err:?}")),
		},
		Err(err) => fail(format!("plugin render failed: {err:?}")),
	}
}

/// Test-only crash hooks (deterministic crash/restart acceptance tests).
struct CrashHooks {
	/// Crash on every job.
	always: bool,
	/// Crash on the first job of each process unless the marker file
	/// exists (create it before crashing, so a respawned host renders).
	once_marker: Option<PathBuf>,
}

impl CrashHooks {
	/// Parse the test-only crash hooks out of the host's arguments.
	///
	/// `args` is the host argv (program name and mode flag included): every
	/// `--ofx-crash-always` sets `always` and every `--ofx-crash-once <path>` takes the
	/// following argument as `once_marker` (ignored when it is the last argument).
	/// Unknown arguments are skipped, so the parser tolerates the worker's other
	/// flags.
	///
	/// Parsing does not validate the marker path; `once_marker` is only consulted by
	/// `maybe_crash`, which aborts on the first job of the process and writes the
	/// marker so the respawned host renders.
	fn from_args(args: &[String]) -> Self {
		let mut hooks = CrashHooks {
			always: false,
			once_marker: None,
		};
		let mut i = 0usize;
		while i < args.len() {
			match args[i].as_str() {
				"--ofx-crash-always" => hooks.always = true,
				"--ofx-crash-once" if i + 1 < args.len() => {
					hooks.once_marker = Some(PathBuf::from(&args[i + 1]));
					i += 1;
				}
				_ => {}
			}
			i += 1;
		}
		hooks
	}

	fn maybe_crash(&self) {
		if self.always {
			process::abort();
		}
		if let Some(path) = &self.once_marker {
			if !path.exists() {
				let _ = fs::write(path, b"ofx-host-crashed");
				process::abort();
			}
		}
	}
}

/// The `--ofx-host` main loop; returns the process exit code (`0`).
///
/// `args` is the full argv, program name and `--ofx-host` included; unknown flags
/// are ignored and the test-only `--ofx-crash-*` hooks are parsed from it. The
/// function installs the same plugin runtime the render workers use (the render
/// executor and the identifier-keyed instance factory, so `handle_job` can
/// resolve and run plugins), scans the plugin cache once (a scan failure is
/// logged, not fatal) and installs the progress reporter factory.
///
/// stdin is read on its own thread; the loop then serves the control protocol:
/// `handshake` attaches the shm pools, `ofx_job` renders one job and emits its
/// `ofx_result`, `plugin_cancel` sets the sticky cancel flag, `shutdown` ends the
/// loop, and anything else (or invalid JSON) emits an `error` reply. Protocol
/// failures are reported as messages, never as a non-zero exit code.
pub fn ofx_host_main(args: &[String]) -> i32 {
	// The same plugin runtime the render workers install: the executor
	// (so `plugin_executor` is callable) and the identifier-keyed instance
	// factory (so jobs resolve their own instances).
	node_factory::install_render_executor();
	if let Err(err) = host::Host::global().cache.scan() {
		eprintln!("ofx-host: plugin scan failed: {err}");
	}
	install_progress_factory();
	let crash = CrashHooks::from_args(args);

	// stdin runs on its own thread (cancel must be observed mid-render);
	// the main loop consumes the forwarded control messages.
	let control = spawn_stdin_reader();
	let mut pools: Option<HostPools> = None;
	while let Ok(line) = control.recv() {
		let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
			emit(&error_message("invalid JSON message", None));
			continue;
		};
		match msg.get("type").and_then(|t| t.as_str()) {
			Some(TYPE_HANDSHAKE) => match attach_pools(&msg) {
				Ok(attached) => pools = Some(attached),
				Err(err) => emit(&error_message(&err, None)),
			},
			Some(TYPE_OFX_JOB) => match &pools {
				Some(pools) => {
					crash.maybe_crash();
					let response = handle_job(msg, pools);
					emit(&response);
				}
				None => emit(&error_message("ofx_job before handshake", None)),
			},
			Some(TYPE_PLUGIN_CANCEL) => OFX_CANCEL.store(true, Ordering::Relaxed),
			Some(TYPE_SHUTDOWN) => break,
			other => emit(&error_message(
				&format!("unknown message type: {}", other.unwrap_or("<missing>")),
				None,
			)),
		}
	}
	0
}

#[cfg(test)]
mod tests;
