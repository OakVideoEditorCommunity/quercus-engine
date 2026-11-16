use super::*;
use crate::worker::ipc::{
	FrameSlotPool, SharedMemoryRegion, ShmMode, WireEffectParam, WireMontageClip,
	WireMontageEffect, WireNodeValue,
};
use quercus_core::colormath::{OutputColorSpec, WorkingColorSpace};
use quercus_node::id::NodeId;
use quercus_node::project::Project;
use quercus_node::sequence::SequenceBehavior;
use quercus_node::track::TrackListBehavior;
use serde_json::json;
use std::collections::HashMap;
use std::ptr;

/// Serializes the tests that mutate the process-global pipeline color
/// settings (the two pre-existing adopt/sync tests included).
static COLOR_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Saves the process-global pipeline color settings and restores them on
/// drop, so a panicking assertion cannot leak the override into the next
/// serialized test. All users hold [`COLOR_TEST_LOCK`].
struct ColorSettingsGuard {
	working: quercus_core::colormath::WorkingColorSpace,
	output: quercus_core::colormath::OutputColorSpec,
}

impl ColorSettingsGuard {
	fn capture() -> ColorSettingsGuard {
		ColorSettingsGuard {
			working: quercus_core::color::pipeline_working_space(),
			output: quercus_core::color::pipeline_output_spec(),
		}
	}
}

impl Drop for ColorSettingsGuard {
	fn drop(&mut self) {
		quercus_core::color::set_pipeline_color_settings(self.working, self.output);
	}
}

/// Test-only RAII environment override: restores the previous value (or
/// absence) on drop, so a panicking assertion cannot leak a crash-mode
/// override into the next serialized test.
struct EnvGuard {
	name: &'static str,
	previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
	fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
		let previous = std::env::var_os(name);
		unsafe { std::env::set_var(name, value) };
		Self { name, previous }
	}
}

impl Drop for EnvGuard {
	fn drop(&mut self) {
		match self.previous.take() {
			Some(value) => unsafe { std::env::set_var(self.name, value) },
			None => unsafe { std::env::remove_var(self.name) },
		}
	}
}

fn test_key(name: &str) -> String {
	static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
	let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	SharedMemoryRegion::make_key(i64::from(std::process::id()), (n & 0x7FFF) as i32)
		+ &format!("-w-{name}")
}

/// The "parent" side of a handshake: create an output segment holding a
/// pool, optionally an input segment, and return the handshake message
/// plus the owner regions (kept alive by the caller).
fn parent_side(
	slots: i32,
	slot_bytes: i64,
	input: bool,
) -> (Value, SharedMemoryRegion, Option<SharedMemoryRegion>) {
	let out_key = test_key("out");
	let out_bytes = FrameSlotPool::bytes_needed(slots as u32, slot_bytes as usize);
	let mut out_region = SharedMemoryRegion::new();
	let opened = out_region.open(&out_key, out_bytes, ShmMode::Create);
	let open_error = out_region.error();
	assert!(opened, "{open_error}");
	// SAFETY: live mapping sized by bytes_needed.
	let _pool =
		unsafe { FrameSlotPool::create(out_region.data(), slots as u32, slot_bytes as usize) };

	let (in_key, _in_bytes, in_region) = if input {
		let in_key = test_key("in");
		let in_bytes = FrameSlotPool::bytes_needed(slots as u32, slot_bytes as usize);
		let mut in_region = SharedMemoryRegion::new();
		assert!(in_region.open(&in_key, in_bytes, ShmMode::Create));
		// SAFETY: live mapping.
		let _ =
			unsafe { FrameSlotPool::create(in_region.data(), slots as u32, slot_bytes as usize) };
		(Some(in_key), Some(in_bytes), Some(in_region))
	} else {
		(None, None, None)
	};

	let hs = json!({
		"type": "handshake",
		"protocol_version": PROTOCOL_VERSION,
		"shm_key": out_key,
		"input_shm_key": in_key.unwrap_or_default(),
		"input_slots": if input { slots } else { 0 },
		"output_slots": slots,
		"slot_data_bytes": slot_bytes,
		"input_slot_data_bytes": if input { slot_bytes } else { 0 },
	});
	(hs, out_region, in_region)
}

#[test]
fn no_backend_detection_matches_cpp() {
	assert!(is_no_backend(""));
	assert!(is_no_backend("none"));
	assert!(is_no_backend("NONE"));
	assert!(!is_no_backend("opengl"));
	assert!(!is_no_backend("vulkan"));
}

#[test]
fn none_backend_session_has_no_renderer_but_serves_messages() {
	let mut s = WorkerSession::create("none").unwrap();
	assert!(!s.has_renderer());
	let resp = s.handle_line(r#"{"type":"shutdown"}"#);
	assert!(resp.is_none());
	assert!(s.shutdown_requested());
}

/// The M15 headless "cpu" backend: no renderer, but the session stays
/// fully operational (generated frames render through CPU eval).
#[test]
fn cpu_backend_session_is_headless_but_operational() {
	assert!(is_cpu_backend("cpu"));
	assert!(is_cpu_backend("CPU"));
	assert!(!is_cpu_backend("auto"));
	let mut s = WorkerSession::create("cpu").unwrap();
	assert!(!s.has_renderer(), "cpu means no GPU renderer");
	assert!(!s.shutdown_requested());
	assert!(s.handle_line(r#"{"type":"shutdown"}"#).is_none());
	assert!(s.shutdown_requested());
}

#[test]
fn startup_handshake_is_protocol_version_1_with_empty_geometry() {
	let s = WorkerSession::create("none").unwrap();
	let hs = s.startup_handshake();
	assert_eq!(
		hs,
		json!({
			"type": "handshake",
			"protocol_version": 1,
			"shm_key": "",
			"input_shm_key": "",
			"input_slots": 0,
			"output_slots": 0,
			"slot_data_bytes": 0,
			"input_slot_data_bytes": 0,
		})
	);
}

#[test]
fn malformed_line_yields_error_response() {
	let mut s = WorkerSession::create("none").unwrap();
	let resp = s.handle_line("this is not json").unwrap();
	assert_eq!(resp["type"], "error");
	assert_eq!(resp["message"], "malformed control message");
}

#[test]
fn unknown_message_type_yields_error_response() {
	let mut s = WorkerSession::create("none").unwrap();
	let resp = s.handle_line(r#"{"type":"frobnicate"}"#).unwrap();
	assert_eq!(resp["message"], "unknown message type: frobnicate");
}

#[test]
fn cancel_and_shutdown_produce_no_response() {
	let mut s = WorkerSession::create("none").unwrap();
	assert!(s.handle_line(r#"{"type":"cancel","ticket":5}"#).is_none());
	assert!(s.handle_line(r#"{"type":"shutdown"}"#).is_none());
	assert!(s.shutdown_requested());
}

#[test]
fn handshake_wrong_protocol_version() {
	let mut s = WorkerSession::create("none").unwrap();
	let resp = s
			.handle_line(
				r#"{"type":"handshake","protocol_version":99,"shm_key":"k","output_slots":1,"slot_data_bytes":16}"#,
			)
			.unwrap();
	assert_eq!(resp["message"], "unsupported protocol version 99");
}

#[test]
fn handshake_missing_geometry() {
	let mut s = WorkerSession::create("none").unwrap();
	let resp = s
		.handle_line(r#"{"type":"handshake","protocol_version":1}"#)
		.unwrap();
	assert_eq!(
		resp["message"],
		"handshake missing output shared-memory geometry"
	);
}

#[test]
fn handshake_attaches_real_output_pool() {
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, out_region, _in) = parent_side(4, 4096, false);
	let resp = s.handle_line(&hs.to_string()).expect("hello_caps response");
	// Protocol v2: a successful attach answers the geometry handshake
	// with the capability announcement.
	assert_eq!(resp["type"], crate::worker::ipc::TYPE_HELLO_CAPS);
	assert_eq!(resp["protocol_version"], PROTOCOL_VERSION);
	let formats: Vec<i64> = resp["formats"]
		.as_array()
		.unwrap()
		.iter()
		.map(|v| v.as_i64().unwrap())
		.collect();
	assert!(formats.contains(&(PixelFormat::F32 as i64)));
	assert!(formats.contains(&(SLOT_FORMAT_BGRA8 as i64)));
	assert_eq!(resp["max_slot_bytes"], 4096);
	// The session now holds a real attached pool with the parent's
	// geometry.
	let out_pool = s.output_pool.as_ref().unwrap();
	assert_eq!(out_pool.slot_count(), 4);
	assert_eq!(out_pool.slot_data_bytes(), 4096);

	// The two views share the same rings, not copies: the parent pops a
	// free slot and the worker's pool sees the ring cursor move; the
	// parent's publish lands in the worker's ready ring.
	// SAFETY: `out_region` is a live mapping containing the pool the
	// session attached to.
	let parent_pool = unsafe { FrameSlotPool::attach(out_region.data()) };
	let mut parent_slot = 0;
	assert!(unsafe { parent_pool.acquire(&mut parent_slot) });
	assert_eq!(parent_slot, 0);
	let mut worker_slot = 0;
	assert!(unsafe { out_pool.acquire(&mut worker_slot) });
	assert_eq!(worker_slot, 1, "worker must see the parent's free-ring pop");

	// SAFETY: `parent_slot` was acquired by the parent; slot_bytes
	// writable.
	unsafe {
		ptr::write_bytes(parent_pool.slot_data(parent_slot), 0xAB, 64);
	}
	assert!(unsafe { parent_pool.publish(parent_slot) });
	let mut consumed = 0;
	assert!(unsafe { out_pool.consume(&mut consumed) });
	assert_eq!(consumed, parent_slot);
	// SAFETY: `consumed` was consumed by the worker's pool.
	assert_eq!(unsafe { *out_pool.slot_data_const(consumed) }, 0xAB);
	// Clean up so the region drop at test end unlinks cleanly.
	unsafe { out_pool.release(consumed) };
	unsafe { out_pool.release(worker_slot) };
}

#[test]
fn handshake_attaches_input_pool_too() {
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(2, 256, true);
	let resp = s.handle_line(&hs.to_string()).expect("hello_caps response");
	assert_eq!(
		resp["type"],
		crate::worker::ipc::TYPE_HELLO_CAPS,
		"handshake failed: {resp}"
	);
	assert!(s.input_pool.is_some());
	let in_pool = s.input_pool.as_ref().unwrap();
	assert_eq!(in_pool.slot_count(), 2);
	assert_eq!(in_pool.slot_data_bytes(), 256);
}

#[test]
fn handshake_attach_failure_reports_error() {
	let mut s = WorkerSession::create("none").unwrap();
	// A key that was never created.
	let resp = s
		.handle_line(
			&json!({
				"type": "handshake",
				"protocol_version": 1,
				"shm_key": format!("olive-rw-{}-missing", std::process::id()),
				"output_slots": 4,
				"slot_data_bytes": 4096,
			})
			.to_string(),
		)
		.unwrap();
	assert_eq!(resp["type"], "error");
	assert!(
		resp["message"]
			.as_str()
			.unwrap()
			.starts_with("failed to attach shared memory: ")
	);
	assert!(s.output_pool.is_none());
}

#[test]
fn handshake_rejects_non_pool_segment() {
	let mut s = WorkerSession::create("none").unwrap();
	// A real segment of the right size that does not contain a pool
	// (zeroed memory → wrong magic). Sized so the attach size check
	// passes and the magic check fires.
	let key = test_key("nopool");
	let bytes = FrameSlotPool::bytes_needed(4, 4096);
	let mut region = SharedMemoryRegion::new();
	assert!(region.open(&key, bytes, ShmMode::Create));
	let resp = s
		.handle_line(
			&json!({
				"type": "handshake",
				"protocol_version": 1,
				"shm_key": key,
				"output_slots": 4,
				"slot_data_bytes": 4096,
			})
			.to_string(),
		)
		.unwrap();
	assert_eq!(
		resp["message"],
		"shared memory does not contain a frame slot pool"
	);
}

#[test]
fn handshake_missing_input_geometry_is_an_error() {
	let mut s = WorkerSession::create("none").unwrap();
	let (mut hs, _out, _in) = parent_side(2, 256, false);
	// Ask for input slots without announcing their geometry.
	hs["input_slots"] = json!(2);
	let resp = s.handle_line(&hs.to_string()).unwrap();
	assert_eq!(
		resp["message"],
		"handshake missing input shared-memory geometry"
	);
}

#[test]
fn load_graph_file_checks_then_real_deserialization() {
	let mut s = WorkerSession::create("none").unwrap();

	let missing = "/definitely/not/a/real/graph.ove";
	let resp = s
		.handle_line(&json!({ "type": "load_graph", "path": missing }).to_string())
		.unwrap();
	assert_eq!(
		resp["message"],
		format!("graph file does not exist: {missing}")
	);

	let empty = std::env::temp_dir().join("quercus_worker_main_test_empty.ove");
	std::fs::write(&empty, b"").unwrap();
	let resp = s
		.handle_line(
			&json!({ "type": "load_graph", "path": empty.display().to_string() }).to_string(),
		)
		.unwrap();
	assert_eq!(
		resp["message"],
		format!("graph file is empty: {}", empty.display())
	);
	let _ = std::fs::remove_file(&empty);

	// A real quercusnode project round-trips through the serializer.
	let project = quercus_node::project::Project::new();
	let xml = quercus_node::serializer::save(&project.lock().unwrap_or_else(|e| e.into_inner()))
		.expect("serialize empty project");
	let real = std::env::temp_dir().join("quercus_worker_main_test_graph.ove");
	std::fs::write(&real, &xml).unwrap();
	let resp = s.handle_line(
		&json!({ "type": "load_graph", "path": real.display().to_string() }).to_string(),
	);
	assert!(resp.is_none(), "unexpected error: {resp:?}");
	let graph = s.graph.as_ref().expect("graph loaded");
	assert!(graph.project.is_some(), "full project deserialized");
	let _ = std::fs::remove_file(&real);

	// The minimal identity-only payload (`{"project_copy":N}`) loads as
	// a copied-project context.
	let ident = std::env::temp_dir().join("quercus_worker_main_test_identity.ove");
	std::fs::write(&ident, r#"{"project_copy":7}"#).unwrap();
	let resp = s.handle_line(
		&json!({ "type": "load_graph", "path": ident.display().to_string() }).to_string(),
	);
	assert!(resp.is_none(), "unexpected error: {resp:?}");
	let graph = s.graph.as_ref().expect("graph loaded");
	assert!(graph.project.is_none());
	assert_eq!(graph.project_copy, 7);
	let _ = std::fs::remove_file(&ident);

	// Garbage that is neither project XML nor identity JSON fails
	// explainably.
	let bad = std::env::temp_dir().join("quercus_worker_main_test_bad.ove");
	std::fs::write(&bad, b"definitely not a graph").unwrap();
	let resp = s
		.handle_line(
			&json!({ "type": "load_graph", "path": bad.display().to_string() }).to_string(),
		)
		.unwrap();
	assert!(
		resp["message"]
			.as_str()
			.unwrap()
			.starts_with("graph deserialization failed: ")
	);
	let _ = std::fs::remove_file(&bad);
}

#[test]
fn load_graph_adopts_pipeline_colors_from_snapshot() {
	use quercus_core::colormath::{OutputColorSpec, WorkingColorSpace};
	let _guard = COLOR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let _colors = ColorSettingsGuard::capture();
	// Reset the process global to something different from the snapshot's
	// settings so the adopt step is observable.
	quercus_core::color::set_pipeline_color_settings(
		WorkingColorSpace::SrgbLegacy,
		OutputColorSpec::default(),
	);
	let project = quercus_node::project::Project::new();
	{
		let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
		guard.set_working_color_space(WorkingColorSpace::AcesCg);
	}
	let xml = quercus_node::serializer::save(&project.lock().unwrap_or_else(|e| e.into_inner()))
		.expect("serialize project");
	let path = std::env::temp_dir().join("quercus_worker_main_test_acescg.ove");
	std::fs::write(&path, &xml).unwrap();

	let mut s = WorkerSession::create("none").unwrap();
	let resp = s.handle_line(
		&json!({ "type": "load_graph", "path": path.display().to_string() }).to_string(),
	);
	assert!(resp.is_none(), "unexpected error: {resp:?}");
	assert_eq!(
		quercus_core::color::pipeline_working_space(),
		WorkingColorSpace::AcesCg,
		"load_graph must adopt the snapshot's working space"
	);
	let _ = std::fs::remove_file(&path);
}

#[test]
fn sync_pipeline_color_from_graph_restores_stale_global() {
	use quercus_core::colormath::WorkingColorSpace;
	let _guard = COLOR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let _colors = ColorSettingsGuard::capture();
	let project = quercus_node::project::Project::new();
	{
		let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
		guard.set_working_color_space(WorkingColorSpace::AcesCg);
	}
	let xml = quercus_node::serializer::save(&project.lock().unwrap_or_else(|e| e.into_inner()))
		.expect("serialize project");
	let path = std::env::temp_dir().join("quercus_worker_main_test_sync.ove");
	std::fs::write(&path, &xml).unwrap();
	let mut s = WorkerSession::create("none").unwrap();
	let resp = s.handle_line(
		&json!({ "type": "load_graph", "path": path.display().to_string() }).to_string(),
	);
	assert!(resp.is_none(), "unexpected error: {resp:?}");

	// Simulate the app committing new project settings: the process
	// global flips immediately, but the resync (load_graph re-broadcast)
	// reaches this worker later. A render ticket in that window must not
	// run under the stale colors.
	quercus_core::color::set_pipeline_color_settings(
		WorkingColorSpace::SrgbLegacy,
		quercus_core::color::pipeline_output_spec(),
	);
	assert!(
		s.sync_pipeline_color_from_graph(),
		"stale global must be refreshed from the loaded project"
	);
	assert_eq!(
		quercus_core::color::pipeline_working_space(),
		WorkingColorSpace::AcesCg,
		"sync must restore the snapshot's working space"
	);
	assert!(
		!s.sync_pipeline_color_from_graph(),
		"no change means no frame-cache invalidation"
	);
	let _ = std::fs::remove_file(&path);
}

#[test]
fn acescg_pipeline_round_trip_preserves_srgb_encoded_midgray() {
	// The ACEScg overexposure bug: F32 bytes produced by the legacy sRGB
	// pass-through (already sRGB-encoded) are re-encoded by the ACEScg
	// output transform, applying the sRGB OETF a second time — 0.5
	// becomes srgb_oetf(0.5) ≈ 0.735. The correct pipeline linearizes
	// the code (sRGB EOTF) into ACEScg and then encodes for output, so
	// the mid-gray comes back near 0.5.
	use quercus_core::colormath::{
		OutputColorSpec, OutputGamut, OutputTransfer, SourcePrimaries, SourceTransfer,
		WorkingColorSpace, acescg_to_output_bytes, decode_to_acescg_bytes, srgb_oetf,
	};
	let spec = OutputColorSpec {
		gamut: OutputGamut::Srgb,
		transfer: OutputTransfer::Srgb,
	};

	// Buggy path: treating the already-encoded code as ACEScg linear.
	let mut double_encoded = [0.5f32, 0.5, 0.5, 1.0];
	quercus_core::colormath::working_to_display_target(
		&mut double_encoded,
		WorkingColorSpace::AcesCg,
		spec,
	);
	assert!(
		(double_encoded[0] - srgb_oetf(0.5)).abs() < 1e-4,
		"mis-encoding 0.5 as ACEScg-linear must yield srgb_oetf(0.5) ≈ 0.735, got {}",
		double_encoded[0]
	);

	// Correct path: decode (sRGB EOTF) → ACEScg → output encode.
	let mut bytes = [0.5f32, 0.5, 0.5, 1.0].map(f32::to_le_bytes).concat();
	decode_to_acescg_bytes(
		&mut bytes,
		1,
		SourcePrimaries::Bt709,
		SourceTransfer::SdrGamma,
	);
	acescg_to_output_bytes(&mut bytes, 1, spec);
	let out = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
	assert!(
		(out - 0.5).abs() < 1e-3,
		"correct pipeline must preserve mid-gray, got {out}"
	);
}

#[test]
fn render_frame_without_pool_reports_error_with_ticket() {
	let mut s = WorkerSession::create("none").unwrap();
	let resp = s
		.handle_line(r#"{"type":"render_frame","ticket":123,"node":"abc"}"#)
		.unwrap();
	assert_eq!(resp["type"], "error");
	assert_eq!(resp["ticket"], 123);
	assert_eq!(
		resp["message"],
		"render_frame: no shared-memory pool attached"
	);
}

#[test]
fn render_frame_v1_renders_generated_frame_into_slot() {
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, out_region, _in) = parent_side(4, 4 * 4 * 16, false);
	let resp = s.handle_line(&hs.to_string()).expect("hello_caps response");
	assert_eq!(resp["type"], crate::worker::ipc::TYPE_HELLO_CAPS);

	// The v1 single-frame path renders the pipeline's generated frame
	// (transparent black F32; format -1 = pipeline default) into an
	// acquired slot and reports it.
	let resp = s
			.handle_line(
				r#"{"type":"render_frame","ticket":123,"time_num":1,"time_den":2,"width":4,"height":4,"format":-1}"#,
			)
			.unwrap();
	assert_eq!(resp["type"], "frame_ready", "unexpected: {resp}");
	assert_eq!(resp["ticket"], 123);
	let slot = resp["slot"].as_i64().unwrap() as u32;

	// The parent (drainer) consumes the published slot and sees the
	// meta the worker wrote.
	let parent_pool = unsafe { FrameSlotPool::attach(out_region.data()) };
	let mut consumed = 0;
	assert!(unsafe { parent_pool.consume(&mut consumed) });
	assert_eq!(consumed, slot);
	let meta = unsafe { &*parent_pool.meta_const(consumed) };
	assert_eq!(meta.id, 123);
	assert_eq!(meta.width, 4);
	assert_eq!(meta.height, 4);
	assert_eq!(meta.format, PixelFormat::F32 as i32);
	assert_eq!(meta.data_size, 4 * 4 * 16);
	// Generated frame: transparent black.
	let data =
		unsafe { std::slice::from_raw_parts(parent_pool.slot_data_const(consumed), 4 * 4 * 16) };
	assert!(data.iter().all(|&b| b == 0));
	unsafe { parent_pool.release(consumed) };
}

#[test]
fn render_batch_stream_renders_generated_frames_and_reports_failures() {
	// The crash-mode env vars are process-wide: serialize with the
	// crash-hook test that mutates them.
	let _guard = GLOBAL_FACTORY_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let mut s = WorkerSession::create("none").unwrap();
	// Slots sized for 8x8 BGRA8.
	let (hs, out_region, _in) = parent_side(4, 8 * 8 * 4, false);
	let resp = s.handle_line(&hs.to_string()).expect("hello_caps response");
	assert_eq!(resp["type"], crate::worker::ipc::TYPE_HELLO_CAPS);

	let batch = json!({
		"type": "render_batch",
		"batch_id": 9,
		"tickets": [
			{ "ticket": 1, "slot": 0, "time_num": 0, "time_den": 1, "width": 8, "height": 8, "format": SLOT_FORMAT_BGRA8, "channels": 4 },
			{ "ticket": 2, "slot": 1, "time_num": 0, "time_den": 1, "width": 0, "height": 8, "format": SLOT_FORMAT_BGRA8, "channels": 4 },
		],
	});
	let mut out: Vec<u8> = Vec::new();
	s.handle_render_batch_stream(&batch.to_string(), &mut out)
		.unwrap();
	let lines: Vec<Value> = String::from_utf8(out)
		.unwrap()
		.lines()
		.map(|l| serde_json::from_str(l).unwrap())
		.collect();
	assert_eq!(lines.len(), 3, "accepted + one reply per ticket");
	assert_eq!(lines[0]["type"], "batch_accepted");
	assert_eq!(lines[0]["batch_id"], 9);
	assert_eq!(lines[1]["type"], "frame_ready");
	assert_eq!(lines[1]["ticket"], 1);
	assert_eq!(lines[1]["slot"], 0);
	assert_eq!(lines[2]["type"], "frame_failed");
	assert_eq!(lines[2]["ticket"], 2);

	// The rendered slot holds opaque black BGRA8 (generated transparent
	// black F32 converted: alpha 0).
	let parent_pool = unsafe { FrameSlotPool::attach(out_region.data()) };
	let mut consumed = 0;
	assert!(unsafe { parent_pool.consume(&mut consumed) });
	assert_eq!(consumed, 0);
	let meta = unsafe { &*parent_pool.meta_const(consumed) };
	assert_eq!(meta.id, 1);
	assert_eq!(meta.format, SLOT_FORMAT_BGRA8);
	assert_eq!(meta.linesize, 8 * 4);
	assert_eq!(meta.data_size, 8 * 8 * 4);
	unsafe { parent_pool.release(consumed) };

	// The failed ticket acquired slot 1 but never published it, so it
	// is still owned by the filler side (not in the ready ring) — the
	// ready ring is empty now.
	assert!(!unsafe { parent_pool.consume(&mut consumed) });
}

#[test]
fn render_audio_batch_stream_mixes_silence_into_slot() {
	// Serialize with the crash-hook env test (see render_batch_stream).
	let _guard = GLOBAL_FACTORY_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	// M15 S3: an empty-montage audio range pull (1/24 s at 48 kHz
	// stereo = 2000 frames x 2 ch = 16000 bytes) renders total silence
	// into the assigned slot and reports frame_ready with the audio
	// slot meta.
	let mut s = WorkerSession::create("none").unwrap();
	let slot_bytes = 2000 * 2 * 4;
	let (hs, out_region, _in) = parent_side(4, slot_bytes as i64, false);
	let resp = s.handle_line(&hs.to_string()).expect("hello_caps response");
	assert_eq!(resp["type"], crate::worker::ipc::TYPE_HELLO_CAPS);

	let batch = json!({
		"type": "render_audio_batch",
		"batch_id": 3,
		"tickets": [
			{
				"ticket": 11,
				"slot": 0,
				"time_num": 0,
				"time_den": 1,
				"duration_num": 1,
				"duration_den": 24,
				"sample_rate": 48000,
				"channel_layout": 0x3,
				"channels": 2,
			},
			{
				"ticket": 12,
				"slot": 1,
				"time_num": 0,
				"time_den": 1,
				"duration_num": 1,
				"duration_den": 24,
				"sample_rate": 0,
				"channel_layout": 0x3,
				"channels": 2,
			},
		],
	});
	let mut out: Vec<u8> = Vec::new();
	s.handle_render_audio_batch_stream(&batch.to_string(), &mut out)
		.unwrap();
	let lines: Vec<Value> = String::from_utf8(out)
		.unwrap()
		.lines()
		.map(|l| serde_json::from_str(l).unwrap())
		.collect();
	assert_eq!(lines.len(), 3, "accepted + one reply per ticket");
	assert_eq!(lines[0]["type"], "batch_accepted");
	assert_eq!(lines[0]["batch_id"], 3);
	assert_eq!(lines[1]["type"], "frame_ready");
	assert_eq!(lines[1]["ticket"], 11);
	assert_eq!(lines[1]["slot"], 0);
	assert_eq!(lines[2]["type"], "frame_failed");
	assert_eq!(lines[2]["ticket"], 12);

	// The rendered slot holds the audio meta + silent samples.
	let parent_pool = unsafe { FrameSlotPool::attach(out_region.data()) };
	let mut consumed = 0;
	assert!(unsafe { parent_pool.consume(&mut consumed) });
	assert_eq!(consumed, 0);
	let meta = unsafe { &*parent_pool.meta_const(consumed) };
	assert_eq!(meta.id, 11);
	assert_eq!(meta.format, SLOT_FORMAT_AUDIO_F32);
	assert_eq!(meta.width, 48000, "width carries the sample rate");
	assert_eq!(meta.height, 0);
	assert_eq!(meta.channel_count, 2);
	assert_eq!(meta.linesize, 2 * 4);
	assert_eq!(meta.data_size, slot_bytes as i32);
	let data =
		unsafe { std::slice::from_raw_parts(parent_pool.slot_data_const(consumed), slot_bytes) };
	assert!(data.iter().all(|&b| b == 0), "empty montage is silence");
	// The samples parse back as 2000 stereo frames.
	let parsed: Vec<f32> = data
		.as_chunks::<4>()
		.0
		.iter()
		.map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
		.collect();
	assert_eq!(parsed.len(), 2000 * 2);
	assert!(parsed.iter().all(|&v| v == 0.0));
	unsafe { parent_pool.release(consumed) };

	// The failed ticket (bad sample rate) acquired slot 1 but never
	// published it.
	assert!(!unsafe { parent_pool.consume(&mut consumed) });
}

#[test]
fn render_audio_batch_rejects_oversized_range() {
	// Serialize with the crash-hook env test (see render_batch_stream).
	let _guard = GLOBAL_FACTORY_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	// A range longer than the slot can hold must fail with frame_failed
	// (never a buffer overflow into the next slot).
	let mut s = WorkerSession::create("none").unwrap();
	// Slot sized for 1/48 s of stereo audio (2000 bytes).
	let (hs, out_region, _in) = parent_side(4, 2000, false);
	let resp = s.handle_line(&hs.to_string()).expect("hello_caps response");
	assert_eq!(resp["type"], crate::worker::ipc::TYPE_HELLO_CAPS);

	let batch = json!({
		"type": "render_audio_batch",
		"batch_id": 4,
		"tickets": [
			{
				"ticket": 21,
				"slot": 0,
				"time_num": 0,
				"time_den": 1,
				"duration_num": 1,
				"duration_den": 24,
				"sample_rate": 48000,
				"channel_layout": 0x3,
				"channels": 2,
			},
		],
	});
	let mut out: Vec<u8> = Vec::new();
	s.handle_render_audio_batch_stream(&batch.to_string(), &mut out)
		.unwrap();
	let lines: Vec<Value> = String::from_utf8(out)
		.unwrap()
		.lines()
		.map(|l| serde_json::from_str(l).unwrap())
		.collect();
	assert_eq!(lines[0]["type"], "batch_accepted");
	assert_eq!(lines[1]["type"], "frame_failed");
	assert_eq!(lines[1]["ticket"], 21);
	let error_text = lines[1]["error"].as_str().unwrap().to_string();
	assert!(
		error_text.contains("needs 16000 bytes"),
		"oversized range reported: {error_text}"
	);
	// Slot 0 acquired but never published.
	let parent_pool = unsafe { FrameSlotPool::attach(out_region.data()) };
	let mut consumed = 0;
	assert!(!unsafe { parent_pool.consume(&mut consumed) });
}

// ---- quercus-worker's in-process session tests (M14 R2: folded from the
// ---- former src/session.rs mirror; the facade's production session
// ---- tests above cover the rest) -------------------------------------

#[test]
fn session_starts_without_pools() {
	let s = WorkerSession::create("none").unwrap();
	assert!(s.output_pool.is_none());
	assert!(s.input_pool.is_none());
	assert!(!s.shutdown_requested());
}

#[test]
fn non_object_json_yields_error_response() {
	let mut s = WorkerSession::create("none").unwrap();
	let resp = s.handle_line("[1,2,3]").unwrap();
	assert_eq!(resp["message"], "malformed control message");
}

#[test]
fn missing_type_field_yields_unknown_error() {
	let mut s = WorkerSession::create("none").unwrap();
	let resp = s.handle_line(r#"{"hello":1}"#).unwrap();
	assert_eq!(resp["message"], "unknown message type: ");
}

#[test]
fn handshake_bad_json_shape_is_invalid_handshake() {
	let mut s = WorkerSession::create("none").unwrap();
	let resp = s
		.handle_line(r#"{"type":"handshake","protocol_version":"x"}"#)
		.unwrap();
	assert_eq!(resp["message"], "invalid handshake message");
}

// ---- M16 R2 branch-coverage additions ---------------------------------

/// One render-batch ticket with the pipeline defaults for everything
/// the tests do not care about.
fn batch_spec(ticket: i64, slot: i32, width: i32, height: i32, format: i32) -> BatchTicketSpec {
	BatchTicketSpec {
		ticket,
		slot,
		time_num: 0,
		time_den: 1,
		width,
		height,
		format,
		channels: 4,
		..Default::default()
	}
}

/// A project holding one empty sequence viewer; returns the project,
/// the viewer's loaded id and the project uuid.
fn sequence_project() -> (Arc<Mutex<Project>>, NodeId, String) {
	let project = Project::new();
	let seq;
	{
		let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
		let (core, behavior) = SequenceBehavior::create();
		seq = guard.graph.add_node(core, behavior);
	}
	let uuid = project
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.uuid
		.clone();
	(project, seq, uuid)
}

#[test]
fn renderer_creation_falls_back_or_succeeds_headless() {
	// On a GPU-less host the dynamic -> direct-OpenGL fallback fails and
	// the session continues headless (M16 S1); on a GPU host it simply
	// initializes. Either is a valid production outcome — creation must
	// never error out of `WorkerSession::create`, and the session's
	// renderer flag must agree with what the factory can do on this host
	// (the second `create` is the availability oracle; the renderer flag
	// is not hand-rolled anywhere else).
	let session = WorkerSession::create("auto").expect("session creation is tolerant");
	assert_eq!(
		session.has_renderer(),
		Renderer::create("auto").is_ok(),
		"the session carries a renderer exactly when the factory can create one"
	);
}

#[test]
fn renderer_is_open_gl_reports_the_backend_kind() {
	let gl = Renderer {
		inner: DisplayRenderer::new(BackendKind::Gl),
	};
	assert!(gl.is_open_gl());
	let vk = Renderer {
		inner: DisplayRenderer::new(BackendKind::Vulkan),
	};
	assert!(!vk.is_open_gl());
}

#[test]
fn initialize_runtime_second_call_is_a_noop() {
	let mut s = WorkerSession::create("none").unwrap();
	s.runtime_initialized = true;
	assert!(s.initialize_runtime(), "already initialized");
}

#[test]
fn worker_progress_events_flush_in_order_and_cancel_is_sticky() {
	let _guard = GLOBAL_FACTORY_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	WORKER_PROGRESS_EVENTS
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clear();
	WORKER_PLUGIN_CANCEL.store(false, Ordering::Relaxed);
	install_worker_progress_factory();
	assert!(
		quercus_ofx_plugin::progress::has_reporter_factory(),
		"the worker factory must be installed for the plugin progress suite"
	);
	// Drive progressStart -> worker factory -> update -> progressEnd
	// through the plugin progress suite, exactly like a real render.
	quercus_ofx_plugin::suites::progress::set_current(Some(
		quercus_ofx_plugin::progress::ProgressReporter::silent(),
	));
	let v2 = quercus_ofx_plugin::suites::progress::suite_v2();
	let v1 = quercus_ofx_plugin::suites::progress::suite_v1();
	let label = std::ffi::CString::new("render").unwrap();
	let message = std::ffi::CString::new("frame 1").unwrap();
	// SAFETY: the suite takes the null handle by contract; the C strings
	// outlive the calls.
	unsafe {
		assert_eq!(
			(v2.start)(std::ptr::null_mut(), label.as_ptr(), message.as_ptr()),
			quercus_ofx_plugin::suites::status::OK
		);
		assert_eq!(
			(v2.update)(std::ptr::null_mut(), 0.25),
			quercus_ofx_plugin::suites::status::OK
		);
		// progressEnd is forwarded by the v1 suite (v2's end is a no-op).
		assert_eq!(
			(v1.end)(std::ptr::null_mut()),
			quercus_ofx_plugin::suites::status::OK
		);
	}
	quercus_ofx_plugin::suites::progress::set_current(None);

	let mut out: Vec<u8> = Vec::new();
	flush_worker_progress(&mut out);
	let events: Vec<Value> = String::from_utf8(out)
		.unwrap()
		.lines()
		.map(|line| serde_json::from_str(line).unwrap())
		.collect();
	assert_eq!(events.len(), 3, "start + update + end");
	assert_eq!(events[0]["type"], crate::worker::ipc::TYPE_PLUGIN_PROGRESS);
	assert_eq!(events[0]["label"], "render");
	assert_eq!(events[0]["message"], "frame 1");
	assert_eq!(events[0]["fraction"], 0.0);
	assert_eq!(events[1]["fraction"], 0.25);
	assert_eq!(events[2]["fraction"], 1.0);

	// plugin_cancel is sticky until a fresh progressStart resets it.
	let mut s = WorkerSession::create("none").unwrap();
	assert!(s.handle_line(r#"{"type":"plugin_cancel"}"#).is_none());
	assert!(WORKER_PLUGIN_CANCEL.load(Ordering::Relaxed));
	WORKER_PLUGIN_CANCEL.store(false, Ordering::Relaxed);
}

/// A writer whose pipe is already closed.
struct FailingWriter;

impl Write for FailingWriter {
	fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
		Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
	}
	fn flush(&mut self) -> io::Result<()> {
		Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
	}
}

#[test]
fn flush_worker_progress_breaks_on_the_first_write_error() {
	let _guard = GLOBAL_FACTORY_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	WORKER_PROGRESS_EVENTS
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clear();
	push_worker_progress(0.1, "a", "b");
	push_worker_progress(0.2, "c", "d");
	let mut failing = FailingWriter;
	flush_worker_progress(&mut failing);
	// The buffer is drained even though the pipe failed.
	let mut out: Vec<u8> = Vec::new();
	flush_worker_progress(&mut out);
	assert!(out.is_empty());
}

#[test]
fn crash_hook_env_error_paths_are_inert() {
	// The env vars are process-wide; the batch tests that call the hook
	// take this same lock. The `EnvGuard`s restore the environment even
	// if an assertion below fails (a leaked crash-mode override could
	// abort a later serialized test).
	let _guard = GLOBAL_FACTORY_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let session = WorkerSession::create("none").unwrap();

	let marker = std::env::temp_dir().join(format!(
		"quercus_worker_crash_marker_{}",
		std::process::id()
	));
	let _ = std::fs::remove_file(&marker);
	let _marker_env = EnvGuard::set("QUERCUS_WORKER_CRASH_MARKER", &marker);

	// A non-numeric ticket id is ignored (no parse, no crash, and no
	// marker file side effect).
	let _ticket = EnvGuard::set("QUERCUS_WORKER_CRASH_ON_TICKET", "not-a-number");
	session.maybe_crash_for_testing(1);
	assert!(
		!marker.exists(),
		"a non-numeric ticket never reaches the marker write"
	);
	drop(_ticket);

	// A matching ticket whose one-shot marker already exists does not
	// crash again (the restarted worker renders for real) and leaves the
	// marker untouched.
	std::fs::write(&marker, b"crashed").unwrap();
	let _ticket = EnvGuard::set("QUERCUS_WORKER_CRASH_ON_TICKET", i64::MIN.to_string());
	session.maybe_crash_for_testing(i64::MIN);
	assert_eq!(
		std::fs::read(&marker).unwrap(),
		b"crashed".as_slice(),
		"the existing marker is left untouched"
	);
	session.maybe_crash_for_testing(7); // different ticket: early return
	assert_eq!(
		std::fs::read(&marker).unwrap(),
		b"crashed".as_slice(),
		"a mismatching ticket writes nothing"
	);

	let _ = std::fs::remove_file(&marker);
}

#[test]
fn handshake_input_attach_failure_reports_error() {
	let mut s = WorkerSession::create("none").unwrap();
	let (mut hs, _out, _in) = parent_side(2, 256, false);
	hs["input_slots"] = json!(2);
	hs["input_slot_data_bytes"] = json!(256);
	hs["input_shm_key"] = json!(format!("olive-rw-{}-missing-in", std::process::id()));
	let resp = s.handle_line(&hs.to_string()).unwrap();
	assert_eq!(resp["type"], "error");
	assert!(
		resp["message"]
			.as_str()
			.unwrap()
			.starts_with("failed to attach input shared memory: "),
		"{resp}"
	);
}

#[test]
fn handshake_rejects_non_pool_input_segment() {
	let mut s = WorkerSession::create("none").unwrap();
	let (mut hs, _out, _in) = parent_side(2, 256, false);
	let key = test_key("nopool-in");
	let bytes = FrameSlotPool::bytes_needed(2, 256);
	let mut region = SharedMemoryRegion::new();
	assert!(region.open(&key, bytes, ShmMode::Create));
	hs["input_slots"] = json!(2);
	hs["input_slot_data_bytes"] = json!(256);
	hs["input_shm_key"] = json!(key);
	let resp = s.handle_line(&hs.to_string()).unwrap();
	assert_eq!(
		resp["message"],
		"input shared memory does not contain a frame slot pool"
	);
}

#[test]
fn load_graph_rejects_bad_shape_and_unreadable_files() {
	let mut s = WorkerSession::create("none").unwrap();
	let resp = s.handle_line(r#"{"type":"load_graph","path":42}"#).unwrap();
	assert_eq!(resp["message"], "invalid load_graph message");

	// A non-empty file that is not valid UTF-8: it passes the metadata
	// checks but cannot be read as a project.
	let unreadable = std::env::temp_dir().join(format!(
		"quercus_worker_unreadable_{}.ove",
		std::process::id()
	));
	std::fs::write(&unreadable, [0xFFu8, 0xFE, 0x00, 0x80]).unwrap();
	let resp = s
		.handle_line(
			&json!({ "type": "load_graph", "path": unreadable.display().to_string() }).to_string(),
		)
		.unwrap();
	assert!(
		resp["message"]
			.as_str()
			.unwrap()
			.starts_with("graph file unreadable: "),
		"{resp}"
	);
	let _ = std::fs::remove_file(&unreadable);

	// The identity-only payload still lands as a graph context.
	let ident = std::env::temp_dir().join(format!(
		"quercus_worker_identity_{}.ove",
		std::process::id()
	));
	std::fs::write(&ident, r#"{"project_copy":11}"#).unwrap();
	assert!(
		s.handle_line(
			&json!({ "type": "load_graph", "path": ident.display().to_string() }).to_string(),
		)
		.is_none()
	);
	let graph = s.graph.as_ref().expect("identity graph loaded");
	assert!(graph.project.is_none());
	assert_eq!(graph.project_copy, 11);
	let _ = std::fs::remove_file(&ident);
}

#[test]
fn render_frame_maps_every_supported_pixel_format() {
	let mut s = WorkerSession::create("none").unwrap();
	// Five slots so each render can publish without draining.
	let (hs, _out, _in) = parent_side(5, 64, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	for format in [
		PixelFormat::U8,
		PixelFormat::U10,
		PixelFormat::U16,
		PixelFormat::F16,
		PixelFormat::F32,
	] {
		let line = json!({
			"type": "render_frame",
			"ticket": 100 + format as i64,
			"time_num": 0,
			"time_den": 1,
			"width": 2,
			"height": 2,
			"format": format as i32,
		})
		.to_string();
		let resp = s.handle_line(&line).unwrap();
		assert_eq!(resp["type"], "frame_ready", "{format:?}: {resp}");
	}
}

#[test]
fn render_frame_rejects_invalid_message_and_unsupported_format() {
	let mut s = WorkerSession::create("none").unwrap();
	let resp = s
		.handle_line(r#"{"type":"render_frame","ticket":"nope"}"#)
		.unwrap();
	assert_eq!(resp["message"], "invalid render_frame message");

	let (hs, _out, _in) = parent_side(2, 256, false);
	assert_eq!(
		s.handle_line(&hs.to_string()).unwrap()["type"],
		crate::worker::ipc::TYPE_HELLO_CAPS
	);
	let resp = s
			.handle_line(
				r#"{"type":"render_frame","ticket":9,"time_num":0,"time_den":1,"width":4,"height":4,"format":7}"#,
			)
			.unwrap();
	assert_eq!(resp["message"], "render_frame: unsupported format 7");
	assert_eq!(resp["ticket"], 9);
}

#[test]
fn render_frame_defaults_zero_size_and_rejects_the_oversized_frame() {
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(1, 64, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	// 0x0 means "pipeline default" (1920x1080), which cannot fit the
	// 64-byte slot.
	let resp = s
			.handle_line(
				r#"{"type":"render_frame","ticket":10,"time_num":0,"time_den":1,"width":0,"height":0,"format":-1}"#,
			)
			.unwrap();
	assert_eq!(
		resp["message"],
		"render_frame: frame larger than the shm slot"
	);
}

#[test]
fn render_frame_reports_no_free_slot_on_shutdown() {
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(1, 256, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	s.shutdown_requested = true;
	let resp = s
			.handle_line(
				r#"{"type":"render_frame","ticket":11,"time_num":0,"time_den":1,"width":4,"height":4,"format":-1}"#,
			)
			.unwrap();
	assert_eq!(resp["message"], "render_frame: no free shm slot");
}

#[test]
fn render_frame_reports_ready_ring_full() {
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(1, 256, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	// Fill the single-entry ready ring, then recycle the slot through
	// the free ring so the render can acquire it again.
	let pool = s.output_pool.clone().unwrap();
	// SAFETY: live attached pool; single-threaded test.
	unsafe {
		let mut slot = 0u32;
		assert!(pool.acquire(&mut slot));
		assert!(pool.publish(slot));
		assert!(pool.release(slot));
	}
	let resp = s
			.handle_line(
				r#"{"type":"render_frame","ticket":12,"time_num":0,"time_den":1,"width":4,"height":4,"format":-1}"#,
			)
			.unwrap();
	assert_eq!(resp["message"], "render_frame: ready ring full");
}

#[test]
fn batch_and_audio_batch_reject_invalid_messages() {
	let mut s = WorkerSession::create("none").unwrap();
	let mut out: Vec<u8> = Vec::new();
	s.handle_render_batch_stream(r#"{"type":"render_batch","batch_id":"x"}"#, &mut out)
		.unwrap();
	s.handle_render_audio_batch_stream(r#"{"type":"render_audio_batch","tickets":7}"#, &mut out)
		.unwrap();
	let lines: Vec<Value> = String::from_utf8(out)
		.unwrap()
		.lines()
		.map(|line| serde_json::from_str(line).unwrap())
		.collect();
	assert_eq!(lines.len(), 2);
	assert_eq!(lines[0]["type"], "error");
	assert_eq!(lines[0]["message"], "invalid render_batch message");
	assert_eq!(lines[1]["type"], "error");
	assert_eq!(lines[1]["message"], "invalid render_audio_batch message");
}

#[test]
fn render_ticket_without_pool_and_with_wrong_slot_assignment() {
	let mut s = WorkerSession::create("none").unwrap();
	let spec = batch_spec(1, 0, 4, 4, PixelFormat::F32 as i32);
	assert_eq!(
		s.render_ticket_to_slot(&spec).unwrap_err(),
		"no shared-memory pool attached"
	);

	let (hs, _out, _in) = parent_side(2, 256, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	let spec = batch_spec(1, 7, 4, 4, PixelFormat::F32 as i32);
	assert_eq!(
		s.render_ticket_to_slot(&spec).unwrap_err(),
		"slot assignment mismatch: acquired 0, assigned 7"
	);
}

#[test]
fn render_ticket_reports_a_full_ready_ring() {
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(1, 256, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	let pool = s.output_pool.clone().unwrap();
	// SAFETY: live attached pool; single-threaded test.
	unsafe {
		let mut slot = 0u32;
		assert!(pool.acquire(&mut slot));
		assert!(pool.publish(slot));
		assert!(pool.release(slot));
	}
	let spec = batch_spec(2, 0, 4, 4, PixelFormat::F32 as i32);
	assert_eq!(
		s.render_ticket_to_slot(&spec).unwrap_err(),
		"ready ring full"
	);
}

#[test]
fn render_spec_pixels_rejects_oversized_and_rerenders_short_cache_entries() {
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(1, 64, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	let pool = s.output_pool.clone().unwrap();

	// 4x4 F32 needs 256 bytes; the slot holds 64.
	let spec = batch_spec(3, 0, 4, 4, PixelFormat::F32 as i32);
	let err = s.render_spec_pixels(&spec, &pool).unwrap_err();
	assert!(err.starts_with("frame 4x4 needs 256 bytes"), "{err}");

	// A 2x2 frame fits exactly; a stale entry smaller than this
	// geometry is defensively discarded and re-rendered.
	let spec = batch_spec(4, 0, 2, 2, PixelFormat::F32 as i32);
	let key = crate::worker::framecache::spec_cache_key(&spec);
	s.frame_cache.insert(key.clone(), vec![0u8; 4]);
	s.render_spec_pixels(&spec, &pool).expect("2x2 fits");
	let cached = s.frame_cache.get(&key).expect("re-rendered and memoized");
	assert_eq!(cached.len(), 64);
}

#[test]
fn render_spec_pixels_reports_footage_decode_failures() {
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(2, 64, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	let pool = s.output_pool.clone().unwrap();
	let missing = "/definitely/not/a/real/clip.mp4";

	// F32 slot: the decode error propagates out of `render_f32_into`.
	let spec = BatchTicketSpec {
		footage_file: missing.to_string(),
		..batch_spec(5, 0, 2, 2, PixelFormat::F32 as i32)
	};
	let err = s.render_spec_pixels(&spec, &pool).unwrap_err();
	assert!(err.starts_with("footage decode: "), "{err}");

	// BGRA8 slot: the same failure through the scratch-buffer path.
	let spec = BatchTicketSpec {
		footage_file: missing.to_string(),
		..batch_spec(6, 1, 2, 2, SLOT_FORMAT_BGRA8)
	};
	let err = s.render_spec_pixels(&spec, &pool).unwrap_err();
	assert!(err.starts_with("footage decode: "), "{err}");
}

#[test]
fn render_audio_ticket_without_pool_wrong_slot_and_full_ring() {
	let mut s = WorkerSession::create("none").unwrap();
	assert_eq!(
		s.render_audio_ticket_to_slot(&AudioTicketSpec::default())
			.unwrap_err(),
		"no shared-memory pool attached"
	);

	let (hs, _out, _in) = parent_side(2, 64, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	let spec = AudioTicketSpec {
		slot: 5,
		..Default::default()
	};
	assert_eq!(
		s.render_audio_ticket_to_slot(&spec).unwrap_err(),
		"slot assignment mismatch: acquired 0, assigned 5"
	);

	// One sample frame of stereo audio needs 8 bytes; fill the
	// single-entry ready ring first so the publish fails.
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(1, 8, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	let pool = s.output_pool.clone().unwrap();
	// SAFETY: live attached pool; single-threaded test.
	unsafe {
		let mut slot = 0u32;
		assert!(pool.acquire(&mut slot));
		assert!(pool.publish(slot));
		assert!(pool.release(slot));
	}
	let spec = AudioTicketSpec {
		ticket: 2,
		slot: 0,
		time_num: 0,
		time_den: 1,
		duration_num: 1,
		duration_den: 48000,
		sample_rate: 48000,
		channel_layout: 0x3,
		channels: 2,
		montage: Vec::new(),
		bus_effects: Vec::new(),
	};
	assert_eq!(
		s.render_audio_ticket_to_slot(&spec).unwrap_err(),
		"ready ring full"
	);
}

#[test]
fn audio_ticket_params_maps_montage_and_effects() {
	let s = WorkerSession::create("none").unwrap();
	let spec = AudioTicketSpec {
		ticket: 1,
		slot: 0,
		time_num: 3,
		time_den: 2,
		duration_num: 1,
		duration_den: 4,
		sample_rate: 44100,
		channel_layout: 0x3,
		channels: 2,
		montage: vec![WireMontageClip {
			filename: "clip.mp4".to_string(),
			stream_index: 2,
			in_num: 1,
			in_den: 2,
			out_num: 3,
			out_den: 2,
			media_in_num: 0,
			media_in_den: 1,
			gain: 0.5,
			pan: 0.0,
			fade_in_num: 0,
			fade_in_den: 1,
			fade_out_num: 0,
			fade_out_den: 1,
			audio_effects: vec![],
			effects: vec![WireMontageEffect {
				type_id: "org.quercus.gain".to_string(),
				enabled: true,
				effect_input_id: "Source".to_string(),
				params: vec![WireEffectParam {
					input: "gain".to_string(),
					value: WireNodeValue::Float(2.0),
				}],
			}],
		}],
		bus_effects: Vec::new(),
	};
	let params = s.audio_ticket_params(&spec).expect("valid geometry");
	assert_eq!(params.viewer, 0);
	assert_eq!(params.range.in_(), Rational::new(3, 2));
	assert_eq!(params.range.out(), Rational::new(7, 4));
	assert_eq!(params.sample_rate, 44100);
	assert_eq!(params.channel_layout, 0x3);
	assert_eq!(params.montage.len(), 1);
	let clip = &params.montage[0];
	assert_eq!(clip.filename, "clip.mp4");
	assert_eq!(clip.stream_index, 2);
	assert_eq!(clip.in_time, Rational::new(1, 2));
	assert_eq!(clip.out_time, Rational::new(3, 2));
	assert_eq!(clip.media_in, Rational::new(0, 1));
	assert_eq!(clip.gain, 0.5);
	assert_eq!(clip.effects.len(), 1);
	assert_eq!(clip.effects[0].type_id, "org.quercus.gain");
	assert!(clip.effects[0].enabled);
	assert_eq!(clip.effects[0].effect_input_id.as_deref(), Some("Source"));
	assert_eq!(clip.effects[0].params.len(), 1);
	assert_eq!(clip.effects[0].params[0].0, "gain");
	assert_eq!(
		clip.effects[0].params[0].1,
		quercus_node::value::NodeValue::Float(2.0)
	);
}

#[test]
fn sync_pipeline_color_without_a_project_is_a_noop() {
	let mut s = WorkerSession::create("none").unwrap();
	assert!(!s.sync_pipeline_color_from_graph(), "no graph at all");
	s.graph = Some(LoadedGraph {
		path: "identity".to_string(),
		project: None,
		project_uuid: None,
		id_map: HashMap::new(),
		project_copy: 3,
	});
	assert!(
		!s.sync_pipeline_color_from_graph(),
		"identity-only graphs carry no color settings"
	);
}

#[test]
fn render_spec_pixels_graph_mode_copies_the_sequence_frame() {
	let _color = COLOR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let (project, seq, uuid) = sequence_project();
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(1, 64, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	let pool = s.output_pool.clone().unwrap();

	let mut graph = LoadedGraph {
		path: "graph".to_string(),
		project: Some(project),
		project_uuid: Some(uuid.clone()),
		id_map: HashMap::new(),
		project_copy: 0,
	};
	graph.id_map.insert(7, seq);
	s.graph = Some(graph);

	let spec = BatchTicketSpec {
		viewer_node: 7,
		project_key: uuid,
		..batch_spec(41, 0, 2, 2, PixelFormat::F32 as i32)
	};
	s.render_spec_pixels(&spec, &pool)
		.expect("an empty sequence renders");
	// SAFETY: slot 0 of the attached pool is live and 64 bytes.
	let dst = unsafe { std::slice::from_raw_parts(pool.slot_data_const(0), 64) };
	assert!(
		dst.iter().all(|&b| b == 0),
		"an empty sequence is transparent black"
	);
}

#[test]
fn graph_mode_falls_back_for_mismatched_or_broken_viewers() {
	let _color = COLOR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let (project, seq, uuid) = sequence_project();
	// A second, non-sequence node: mapping a viewer to it makes
	// `render_graph_frame` fail and take the fallback path.
	let track_list;
	{
		let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
		let (core, behavior) = TrackListBehavior::create();
		track_list = guard.graph.add_node(core, behavior);
	}
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(1, 64, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	let pool = s.output_pool.clone().unwrap();

	let mut graph = LoadedGraph {
		path: "graph".to_string(),
		project: Some(project),
		project_uuid: Some(uuid.clone()),
		id_map: HashMap::new(),
		project_copy: 0,
	};
	graph.id_map.insert(7, seq);
	graph.id_map.insert(8, track_list);
	s.graph = Some(graph);

	// (a) A stale project key: the snapshot must not answer it.
	let spec = BatchTicketSpec {
		viewer_node: 7,
		project_key: "another-project".to_string(),
		..batch_spec(51, 0, 2, 2, PixelFormat::F32 as i32)
	};
	s.render_spec_pixels(&spec, &pool)
		.expect("stale identity falls back");

	// (b) A viewer identity that is not even a valid NodeId.
	let spec = BatchTicketSpec {
		viewer_node: u64::MAX,
		project_key: uuid.clone(),
		..spec.clone()
	};
	s.render_spec_pixels(&spec, &pool)
		.expect("absent viewer falls back");

	// (c) A viewer mapped to a non-sequence node: graph render errors.
	let spec = BatchTicketSpec {
		viewer_node: 8,
		project_key: uuid.clone(),
		..spec.clone()
	};
	s.render_spec_pixels(&spec, &pool)
		.expect("broken viewer falls back");

	// (d) A loaded identity-only graph (uuid matches, no project).
	s.graph = Some(LoadedGraph {
		path: "identity".to_string(),
		project: None,
		project_uuid: Some(uuid.clone()),
		id_map: HashMap::new(),
		project_copy: 0,
	});
	let spec = BatchTicketSpec {
		viewer_node: 7,
		project_key: uuid,
		..spec
	};
	s.render_spec_pixels(&spec, &pool)
		.expect("identity-only graph falls back");
}

#[test]
fn warn_graph_fallback_is_one_shot() {
	// Smoke test: the only observable effect of the warning is a single
	// stderr line, and the worker has no test-installable log sink, so
	// the one-shot suppression itself cannot be asserted. What must hold
	// either way: repeated and concurrent calls all return without
	// corrupting shared state (the marker is an AtomicBool and the sink
	// is stderr, so no call may deadlock or panic).
	warn_graph_fallback(123, "first reason");
	warn_graph_fallback(123, "second reason");

	let completed = std::sync::atomic::AtomicUsize::new(0);
	std::thread::scope(|scope| {
		for viewer in 0..8u64 {
			let completed = &completed;
			scope.spawn(move || {
				warn_graph_fallback(viewer, "concurrent reason");
				completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
			});
		}
	});
	assert_eq!(
		completed.load(std::sync::atomic::Ordering::Relaxed),
		8,
		"every concurrent fallback warning must return"
	);
}

#[test]
fn stale_pipeline_colors_are_refreshed_and_clear_the_frame_cache() {
	let _color = COLOR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	// Loop over both starting states so the opposite-selection branch
	// runs both ways despite the shared process-global color state.
	for current in [WorkingColorSpace::SrgbLegacy, WorkingColorSpace::AcesCg] {
		quercus_core::color::set_pipeline_color_settings(current, OutputColorSpec::default());
		let opposite = if current == WorkingColorSpace::SrgbLegacy {
			WorkingColorSpace::AcesCg
		} else {
			WorkingColorSpace::SrgbLegacy
		};
		let project = Project::new();
		{
			let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
			guard.set_working_color_space(opposite);
		}
		let uuid = project
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.uuid
			.clone();
		let mut s = WorkerSession::create("none").unwrap();
		s.graph = Some(LoadedGraph {
			path: "graph".to_string(),
			project: Some(project),
			project_uuid: Some(uuid),
			id_map: HashMap::new(),
			project_copy: 0,
		});
		s.frame_cache.insert("stale".to_string(), vec![1u8; 8]);

		let (hs, _out, _in) = parent_side(1, 64, false);
		assert!(s.handle_line(&hs.to_string()).is_some());
		let pool = s.output_pool.clone().unwrap();
		let spec = batch_spec(61, 0, 2, 2, PixelFormat::F32 as i32);
		s.render_spec_pixels(&spec, &pool)
			.expect("renders under the refreshed colors");
		assert_eq!(
			quercus_core::color::pipeline_working_space(),
			opposite,
			"the loaded graph's colors are adopted"
		);
		assert!(
			s.frame_cache.get("stale").is_none(),
			"a color change invalidates the frame cache"
		);
	}

	quercus_core::color::set_pipeline_color_settings(
		WorkingColorSpace::default(),
		OutputColorSpec::default(),
	);
}

#[test]
fn apply_output_node_is_a_noop_in_the_legacy_working_space() {
	let _color = COLOR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	quercus_core::color::set_pipeline_color_settings(
		WorkingColorSpace::SrgbLegacy,
		OutputColorSpec::default(),
	);
	let mut bytes: Vec<u8> = [0.25f32, 0.5, 0.75, 1.0]
		.iter()
		.flat_map(|value| value.to_le_bytes())
		.collect();
	let before = bytes.clone();
	apply_output_node(&mut bytes, 1);
	assert_eq!(bytes, before, "legacy sRGB is display-referred already");
	quercus_core::color::set_pipeline_color_settings(
		WorkingColorSpace::default(),
		OutputColorSpec::default(),
	);
}

#[test]
fn worker_main_without_renderer_exits_one() {
	// Mirrors quercusengine_worker_main(): an explicit no-renderer backend
	// leaves nothing to evaluate, so the process exits 1 before the
	// control loop.
	assert_eq!(worker_main("none"), 1);
}

// ---- M16 R2 branch-coverage additions (runtime + renderer) ------------

/// The full `initialize_runtime` body on a fresh session: color config,
/// text backends, plugin executor, OFX scan/registration and the
/// process-global progress factory. A plugin scan failure is tolerated,
/// so either outcome is a valid production result.
#[test]
fn initialize_runtime_installs_the_full_stack() {
	// The reporter factory is process-global; serialize with every
	// other factory test (worker and ofx_host share this lock).
	let _guard = GLOBAL_FACTORY_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let _color = COLOR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let mut s = WorkerSession::create("none").unwrap();
	assert!(!s.runtime_initialized);
	assert!(s.initialize_runtime(), "the runtime always initializes");
	assert!(s.runtime_initialized);
	assert!(
		quercus_ofx_plugin::progress::has_reporter_factory(),
		"the worker progress factory is installed"
	);
	// The second call short-circuits before re-installing anything.
	assert!(s.initialize_runtime());
}

/// The dynamic / direct-OpenGL renderer factories: a GPU host
/// initializes one, a GPU-less host fails both — either outcome is a
/// valid production result, so the assertions pin the invariants that
/// must hold either way: a successful dynamic attempt reports the
/// requested backend kind (init resolves adapters internally but never
/// rewrites the kind), a failure names the stage that failed, and the
/// chained `create` succeeds exactly when one of its two stages can,
/// reporting OpenGL exactly when the fallback produced it.
#[test]
fn renderer_create_variants_are_tolerant() {
	match Renderer::create_dynamic("gl") {
		Ok(renderer) => assert!(renderer.is_open_gl(), "the gl request reports OpenGL"),
		Err(error) => assert!(
			error.starts_with("failed to initialize dynamic gl renderer"),
			"the gl failure names the stage: {error}"
		),
	}

	let dynamic_auto = Renderer::create_dynamic("auto");
	let dynamic_metal = Renderer::create_dynamic("metal");
	for (label, attempt) in [("auto", &dynamic_auto), ("metal", &dynamic_metal)] {
		match attempt {
			Ok(renderer) => assert!(!renderer.is_open_gl(), "{label} is not an OpenGL request"),
			Err(error) => assert!(
				error.starts_with(&format!("failed to initialize dynamic {label} renderer")),
				"the {label} failure names the stage: {error}"
			),
		}
	}

	let opengl = Renderer::create_opengl();
	match &opengl {
		Ok(renderer) => assert!(renderer.is_open_gl(), "direct OpenGL reports OpenGL"),
		Err(error) => assert!(
			error.starts_with("failed to initialize direct OpenGL renderer"),
			"the fallback failure names the stage: {error}"
		),
	}

	// `Renderer::create` = dynamic stage, then direct-OpenGL fallback:
	// its outcome is fully determined by the two stages observed above.
	for (request, dynamic) in [("bogus", &dynamic_auto), ("metal", &dynamic_metal)] {
		match Renderer::create(request) {
			Ok(renderer) => {
				assert_eq!(
					renderer.is_open_gl(),
					dynamic.is_err(),
					"{request}: OpenGL is reported exactly when the dynamic stage failed"
				);
				if dynamic.is_err() {
					assert!(
						opengl.is_ok(),
						"{request}: the fallback must have initialized"
					);
				}
			}
			Err(error) => {
				assert!(
					dynamic.is_err() && opengl.is_err(),
					"{request}: a chained error means both stages failed"
				);
				assert!(
					error.contains(&format!("dynamic {request}"))
						&& error.contains("direct OpenGL fallback also failed"),
					"{request}: the chained error names both stages: {error}"
				);
			}
		}
	}
}

/// An F32 cache hit copies the memoized bytes into the slot instead of
/// re-rendering (a miss would produce transparent black).
#[test]
fn render_spec_pixels_f32_cache_hit_copies_without_render() {
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(1, 64, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	let pool = s.output_pool.clone().unwrap();
	let spec = batch_spec(71, 0, 2, 2, PixelFormat::F32 as i32);
	let key = crate::worker::framecache::spec_cache_key(&spec);
	let known: Vec<u8> = (0..64).map(|i| i as u8).collect();
	s.frame_cache.insert(key.clone(), known.clone());
	s.render_spec_pixels(&spec, &pool).expect("cache hit");
	// SAFETY: slot 0 of the attached pool is live and 64 bytes.
	let dst = unsafe { std::slice::from_raw_parts(pool.slot_data_const(0), 64) };
	assert_eq!(dst, &known[..], "the cached F32 bytes land in the slot");
	assert!(
		s.frame_cache.get(&key).is_some(),
		"the entry stays memoized"
	);
}

/// A BGRA8 cache hit runs the output node + format convert on the
/// memoized F32 bytes (a miss would produce transparent black BGRA8).
#[test]
fn render_spec_pixels_bgra8_cache_hit_converts_the_memoized_frame() {
	let _color = COLOR_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	quercus_core::color::set_pipeline_color_settings(
		WorkingColorSpace::SrgbLegacy,
		OutputColorSpec::default(),
	);
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(1, 64, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	let pool = s.output_pool.clone().unwrap();
	let spec = batch_spec(72, 0, 2, 2, SLOT_FORMAT_BGRA8);
	let key = crate::worker::framecache::spec_cache_key(&spec);
	// Four opaque-white F32 pixels.
	let white: Vec<u8> = std::iter::repeat_n(1.0f32.to_le_bytes(), 2 * 2 * 4)
		.flatten()
		.collect();
	s.frame_cache.insert(key.clone(), white);
	s.render_spec_pixels(&spec, &pool).expect("cache hit");
	// SAFETY: slot 0 of the attached pool holds the 2x2 BGRA8 frame.
	let dst = unsafe { std::slice::from_raw_parts(pool.slot_data_const(0), 2 * 2 * 4) };
	assert!(
		dst.iter().all(|&b| b == 255),
		"cached white F32 converts to opaque white BGRA8: {dst:?}"
	);
	quercus_core::color::set_pipeline_color_settings(
		WorkingColorSpace::default(),
		OutputColorSpec::default(),
	);
}

/// The batch path's acquire failure (shutdown) and a render error both
/// surface as `Err` from `render_ticket_to_slot` (the slot is never
/// published in either case).
#[test]
fn render_ticket_to_slot_reports_shutdown_and_render_errors() {
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(2, 256, false);
	assert!(s.handle_line(&hs.to_string()).is_some());

	// Shutdown: `acquire_slot` refuses before touching the rings.
	s.shutdown_requested = true;
	let spec = batch_spec(1, 0, 4, 4, PixelFormat::F32 as i32);
	assert_eq!(
		s.render_ticket_to_slot(&spec).unwrap_err(),
		"no free shm slot (shutdown or timeout)"
	);
	s.shutdown_requested = false;

	// A render failure (missing footage) propagates out of the batch
	// path unchanged.
	let spec = BatchTicketSpec {
		footage_file: "/definitely/not/a/real/clip.mp4".to_string(),
		..batch_spec(2, 0, 2, 2, PixelFormat::F32 as i32)
	};
	let err = s.render_ticket_to_slot(&spec).unwrap_err();
	assert!(err.starts_with("footage decode: "), "{err}");
}

/// The audio batch path's acquire failure: a shutdown session returns
/// "no free shm slot" before mixing anything.
#[test]
fn render_audio_ticket_to_slot_reports_shutdown() {
	let mut s = WorkerSession::create("none").unwrap();
	let (hs, _out, _in) = parent_side(1, 64, false);
	assert!(s.handle_line(&hs.to_string()).is_some());
	s.shutdown_requested = true;
	let spec = AudioTicketSpec {
		ticket: 5,
		slot: 0,
		time_num: 0,
		time_den: 1,
		duration_num: 1,
		duration_den: 48000,
		sample_rate: 48000,
		channel_layout: 0x3,
		channels: 2,
		montage: Vec::new(),
		bus_effects: Vec::new(),
	};
	assert_eq!(
		s.render_audio_ticket_to_slot(&spec).unwrap_err(),
		"no free shm slot (shutdown or timeout)"
	);
}
