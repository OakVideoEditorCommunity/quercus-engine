use super::*;
use crate::worker::JobSchedule;

#[test]
fn slot_bytes_for_formats() {
	// F32 RGBA: 16 bytes per pixel.
	assert_eq!(slot_bytes_for(1920, 1080, 4), 1920 * 1080 * 16);
	// BGRA8: 4 bytes per pixel (the 8.3 MB design figure).
	assert_eq!(
		slot_bytes_for(1920, 1080, SLOT_FORMAT_BGRA8),
		1920 * 1080 * 4
	);
	// U8 RGBA.
	assert_eq!(slot_bytes_for(64, 64, 0), 64 * 64 * 4);
}

#[test]
fn worker_count_policy_is_clamped() {
	// With absurd slot sizes the memory budget clamps to 1.
	let n = default_worker_count(64, 1 << 30); // 64 GiB per worker
	assert_eq!(n, 1);
	// With tiny slots the core policy dominates (>= 1).
	let n = default_worker_count(1, 64);
	assert!(n >= 1);
}

/// The Linux sysfs walk reads total/used per card, picks the first
/// non-zero total, and skips (not aborts on) cards without the
/// attributes — the AMD/Intel query path (amdgpu, i915, Xe expose
/// `mem_info_vram_*`; an iGPU with no dedicated vram reports 0 and is
/// skipped for the discrete card).
#[cfg(target_os = "linux")]
#[test]
fn linux_drm_vram_walk_reads_cards_and_skips_missing() {
	let dir = std::env::temp_dir().join(format!(
		"quercus_vram_fixture_{}_{}",
		std::process::id(),
		std::thread::current().name().unwrap_or("t")
	));
	let _ = std::fs::remove_dir_all(&dir);
	let write = |card: u32, total: Option<u64>, used: u64| {
		let base = dir.join(format!("card{card}")).join("device");
		std::fs::create_dir_all(&base).unwrap();
		if let Some(total) = total {
			std::fs::write(base.join("mem_info_vram_total"), total.to_string()).unwrap();
		}
		std::fs::write(base.join("mem_info_vram_used"), used.to_string()).unwrap();
	};
	// card0: no attributes (a display-only card) -> skipped.
	let card0 = dir.join("card0").join("device");
	std::fs::create_dir_all(&card0).unwrap();
	// card1: 8 GiB total, 2 GiB used -> the winner.
	write(1, Some(8 << 30), 2 << 30);
	// card2: 0 total (iGPU without dedicated memory) -> skipped.
	write(2, Some(0), 0);

	let (free, total) =
		linux_drm_vram_bytes_from(&dir).expect("the walk must find the first non-zero card");
	assert_eq!(total, 8 << 30);
	assert_eq!(free, 6 << 30);

	// card3: a numeric total but no `used` file -> used defaults to 0;
	// card4: an unparsable total -> skipped.
	let card3 = dir.join("card3").join("device");
	std::fs::create_dir_all(&card3).unwrap();
	std::fs::write(card3.join("mem_info_vram_total"), (4u64 << 30).to_string()).unwrap();
	let card4 = dir.join("card4").join("device");
	std::fs::create_dir_all(&card4).unwrap();
	std::fs::write(card4.join("mem_info_vram_total"), "not-a-number").unwrap();
	std::fs::write(card4.join("mem_info_vram_used"), "0").unwrap();
	// Remove the first winner so the walk reaches card3.
	let _ = std::fs::remove_dir_all(dir.join("card1"));
	let (free, total) = linux_drm_vram_bytes_from(&dir).expect("card3 wins");
	assert_eq!(total, 4 << 30);
	assert_eq!(free, 4 << 30, "a missing `used` file counts as zero");

	// No usable card -> None (display-only + zero-total + garbage total).
	for f in [&dir.join("card0"), &dir.join("card3"), &dir.join("card4")] {
		let _ = std::fs::remove_dir_all(f);
	}
	assert!(linux_drm_vram_bytes_from(&dir).is_none());

	let _ = std::fs::remove_dir_all(&dir);
}

/// `per_worker_gpu_budget` follows the pixel-count figure: 1080p is
/// the 1 GiB + 256 MiB baseline, 4K ~4× of it, and higher fps
/// over-provisions slightly (more surfaces in flight).
#[test]
fn per_worker_budget_scales_with_frame_size() {
	let hd = per_worker_gpu_budget((1920, 1080), 24);
	let fhd = per_worker_gpu_budget((1920, 1080), 60);
	let uhd = per_worker_gpu_budget((3840, 2160), 24);
	assert!(uhd > hd * 3, "4K budget must be ~4× 1080p ({uhd} vs {hd})");
	assert!(fhd >= hd, "60 fps must over-provision ({fhd} vs {hd})");
	// 1080p24 = 2 GiB peak (1 GiB × headroom 2) + 256 MiB idle.
	assert_eq!(hd, (2 << 30) + (256 << 20));
}

/// The GPU-vram worker budget scales with the frame's pixel count: a
/// 1080p-sized budget allows the CPU-bound count, a 4K budget only
/// the vram-fitting fraction (the user's "1080p 22 workers but 4K
/// only 5" figure). Pure arithmetic — the vram probe is mocked by
/// calling the budget formula directly.
#[test]
fn gpu_worker_budget_scales_with_frame_size() {
	// Use the real budget formula (headroom included): at 1080p24 the
	// per-worker budget is 2 GiB peak + 256 MiB idle = 2.25 GiB; at
	// 4K it is 4× that (pixel ratio 4) + idle = 8.25 GiB. With 24 GiB
	// free / 10% reserve (21.6 GiB usable): 9 workers at 1080p, 2 at
	// 4K — the pool must shrink sharply on resolution switch.
	let usable = (24u64 << 30) * 9 / 10;
	let n_1080p = usable / per_worker_gpu_budget((1920, 1080), 24);
	let n_4k = usable / per_worker_gpu_budget((3840, 2160), 24);
	let n_60 = usable / per_worker_gpu_budget((1920, 1080), 60);
	let n_24_1080 = usable / per_worker_gpu_budget((1920, 1080), 24);
	assert!(
		n_1080p > n_4k * 3,
		"4K must cut the worker count sharply ({n_1080p} vs {n_4k})"
	);
	assert_eq!(
		n_4k, 2,
		"24 GiB free @ 4K with the headroom budget: 2 workers"
	);
	assert!(
		n_60 < n_24_1080,
		"higher fps must not get MORE workers ({n_60} vs {n_24_1080})"
	);
}

#[test]
fn preview_window_capacity_reserves_one_slot_per_worker() {
	// workers=3 × slots=4 → the window may hold 12-3=9 slots; the
	// reserve keeps interactive/audio tickets dispatchable (the
	// playback-freeze regression guard).
	let config = DispatcherConfig {
		worker_bin: Some(true_bin()),
		workers: 3,
		slots_per_worker: 4,
		width: 64,
		height: 64,
		batch_size: 2,
		..Default::default()
	};
	let dispatcher = ProcessDispatcher::new(config).expect("dispatcher");
	assert_eq!(dispatcher.preview_window_capacity(), 9);
}

#[test]
fn config_normalization_defaults() {
	let c = DispatcherConfig::default().normalize();
	// Geometry defaults resolve here; the adaptive counts stay 0 (auto)
	// and resolve in `ProcessDispatcher::new` where slot_bytes is known.
	assert_eq!(c.width, 1920);
	assert_eq!(c.height, 1080);
	assert_eq!(c.slot_format, SLOT_FORMAT_BGRA8);
	assert_eq!(c.slots_per_worker, 0);
	assert_eq!(c.workers, 0);
	assert_eq!(c.batch_size, 0);
}

#[test]
fn slots_policy_adapts_to_slot_size() {
	// BGRA8 1080p: the full 8 slots (~66 MB per worker segment).
	let bgra8_1080p = slot_bytes_for(1920, 1080, SLOT_FORMAT_BGRA8);
	assert_eq!(default_slots_per_worker(bgra8_1080p), 8);
	// F32 1080p: drops to 4 (~133 MB per worker segment).
	let f32_1080p = slot_bytes_for(1920, 1080, 4);
	assert_eq!(default_slots_per_worker(f32_1080p), 4);
	// F32 4K: 2 slots (the floor).
	let f32_4k = slot_bytes_for(3840, 2160, 4);
	assert_eq!(default_slots_per_worker(f32_4k), 2);
	// Tiny slots: the cap at 8.
	assert_eq!(default_slots_per_worker(16), 8);
}

#[test]
fn batch_size_policy_scales_with_workers_and_slots() {
	// 4 workers x 8 slots: the design 120/4 = 30 caps at the 8 slots.
	assert_eq!(default_batch_size(4, 8), 8);
	// 1 worker x 8 slots: 120/1 = 120 caps at 8.
	assert_eq!(default_batch_size(1, 8), 8);
	// 2 workers x 4 slots: 120/2 = 60 caps at 4.
	assert_eq!(default_batch_size(2, 4), 4);
	// 8 workers x 8 slots: 120/8 = 15 caps at 8.
	assert_eq!(default_batch_size(8, 8), 8);
}

#[test]
fn grown_segment_slots_stay_bounded() {
	// Growing a segment keeps a sane slot count: 8.3 MB slots keep 8;
	// 33 MB slots keep 8 (still within the 256 MiB grown budget);
	// absurd sizes clamp at 2.
	assert_eq!(default_slots_for_bytes(8_300_000, 8), 8);
	assert_eq!(default_slots_for_bytes(33_000_000, 8), 8);
	assert_eq!(default_slots_for_bytes(1 << 30, 8), 2);
}

#[test]
fn copy_counter_counts_only_slot_to_vec() {
	// The zero-copy contract: only `slot_to_vec` bumps the counter —
	// `slot_bytes` (the borrowed view the preview path uses) must not.
	reset_main_heap_frame_copies();
	let key = format!("quercus-procpool-copycounter-{}", std::process::id());
	let view = ShmRegionView::create(&key, 2, 64).expect("shm segment");
	let _borrowed = view.slot_bytes(0);
	assert_eq!(
		main_heap_frame_copies(),
		0,
		"borrowed slot reads stay zero-copy"
	);
	let _copied = view.slot_to_vec(0);
	assert_eq!(
		main_heap_frame_copies(),
		1,
		"slot_to_vec is the one counted copy"
	);
	drop(view);
	SharedMemoryRegion::unlink_key(&key);
	reset_main_heap_frame_copies();
}

/// A worker's `plugin_progress` NDJSON line is forwarded to the
/// registered app callback as (label, message, fraction) — the seam
/// that drives the main-process plugin-progress dialog.
#[test]
fn plugin_progress_line_forwards_to_callback() {
	let _lock = pool_test_lock();
	let config = DispatcherConfig {
		worker_bin: Some(true_bin()),
		workers: 1,
		slots_per_worker: 2,
		width: 16,
		height: 16,
		batch_size: 1,
		..Default::default()
	};
	let dispatcher = ProcessDispatcher::new(config).expect("dispatcher");
	// `new` registered the weak handle (the cancel broadcast seam).
	assert!(dispatcher_slot().lock().unwrap().upgrade().is_some());

	let received: Arc<Mutex<Vec<(String, String, f64)>>> = Arc::new(Mutex::new(Vec::new()));
	set_plugin_progress_cb(Some(Arc::new({
		let received = received.clone();
		move |label, message, fraction| {
			received.lock().unwrap().push((label, message, fraction));
		}
	})));

	// A fake worker handle so on_line has a target (no spawn needed).
	{
		let mut inner = dispatcher.inner.lock().unwrap_or_else(|e| e.into_inner());
		let key = SharedMemoryRegion::make_key(std::process::id() as i64, 999);
		let shm = ShmRegionView::create(&key, 2, 256).expect("shm");
		inner.workers.push(WorkerHandle::shell(0, shm, 2, 256));
	}

	let mut fired = Vec::new();
	{
		let mut inner = dispatcher.inner.lock().unwrap_or_else(|e| e.into_inner());
		dispatcher.on_line(
			&mut inner,
			0,
			r#"{"type":"plugin_progress","label":"render","message":"pass 1","fraction":0.5}"#,
			&mut fired,
		);
	}
	set_plugin_progress_cb(None);

	let events = received.lock().unwrap().clone();
	assert_eq!(events.len(), 1);
	assert_eq!(events[0], ("render".to_string(), "pass 1".to_string(), 0.5));
}

// ---- Branch-coverage fill-ins: pure helpers and synthetic workers ------

/// Serializes the synthetic-process / global-env tests below (they
/// register the process-wide dispatcher slot and mutate env vars).
static POOL_TEST_LOCK: Mutex<()> = Mutex::new(());

fn pool_test_lock() -> MutexGuard<'static, ()> {
	POOL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn shm_key(name: &str) -> String {
	format!("quercus-procpool-ut-{}-{name}", std::process::id())
}

/// The immediate-exit worker binary used by the dispatcher tests:
/// macOS ships `true` in `/usr/bin` (there is no `/bin/true`), so
/// probe the usual locations before falling back to `PATH`.
fn true_bin() -> std::path::PathBuf {
	["/usr/bin/true", "/bin/true"]
		.into_iter()
		.map(std::path::PathBuf::from)
		.find(|p| p.exists())
		.unwrap_or_else(|| std::path::PathBuf::from("true"))
}

fn test_config(workers: usize, slots: u32) -> DispatcherConfig {
	DispatcherConfig {
		worker_bin: Some(true_bin()),
		workers,
		slots_per_worker: slots,
		width: 16,
		height: 16,
		batch_size: 2,
		..Default::default()
	}
}

fn video_params(frame: i64) -> VideoTicketParams {
	VideoTicketParams {
		viewer: 1,
		project: String::new(),
		time: quercus_core::Rational::new(frame, 25),
		force_size: Some((16, 16)),
		force_format: None,
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage: Vec::new(),
		adjustments: Vec::new(),
	}
}

fn test_job(
	sequence: u64,
	frame: i64,
	audio: Option<Arc<AudioTicketParams>>,
	results: &Arc<Mutex<Vec<TicketResult>>>,
) -> Job {
	let results = results.clone();
	Job {
		node_identity: sequence,
		time: quercus_core::Rational::new(frame, 25),
		params: Arc::new(video_params(frame)),
		audio,
		produce: Arc::new(|_, _| {
			Err(Error::Failed(
				"process backend does not use the in-process producer".into(),
			))
		}),
		done: Box::new(move |result| {
			results
				.lock()
				.unwrap_or_else(|e| e.into_inner())
				.push(result);
		}),
		schedule: JobSchedule::seek(),
		cancelled: None,
	}
}

fn pump_until(dispatcher: &ProcessDispatcher, results: &Mutex<Vec<TicketResult>>, expected: usize) {
	let deadline = Instant::now() + Duration::from_secs(120);
	loop {
		dispatcher.poll();
		if results.lock().unwrap_or_else(|e| e.into_inner()).len() >= expected {
			return;
		}
		if Instant::now() > deadline {
			let have = results.lock().unwrap_or_else(|e| e.into_inner()).len();
			panic!("timeout: {have}/{expected} completions");
		}
		std::thread::sleep(Duration::from_millis(5));
	}
}

/// Allocate a slot from `view`'s free ring, stamp `id` into its metadata
/// and publish it — what a worker does before `frame_ready`.
fn publish_slot(view: &ShmRegionView, id: i64) -> u32 {
	let pool = view.pool();
	let mut slot = 0u32;
	unsafe {
		assert!(pool.acquire(&mut slot), "a free slot");
		let meta = pool.meta(slot);
		(*meta).id = id;
		(*meta).data_size = 8;
		assert!(pool.publish(slot), "ready ring has room");
	}
	slot
}

/// Saves an environment variable and restores it on drop (the env is
/// process-global; all users hold [`POOL_TEST_LOCK`]).
struct EnvRestore {
	key: &'static str,
	value: Option<std::ffi::OsString>,
}

impl EnvRestore {
	fn set(key: &'static str, value: &str) -> EnvRestore {
		let saved = std::env::var_os(key);
		std::env::set_var(key, value);
		EnvRestore { key, value: saved }
	}
}

impl Drop for EnvRestore {
	fn drop(&mut self) {
		match self.value.take() {
			Some(value) => std::env::set_var(self.key, value),
			None => std::env::remove_var(self.key),
		}
	}
}

/// The quercus-worker binary: `$QUERCUS_WORKER_BIN`, else the sibling of the
/// test executable under `target/<profile>/`. Tests skip (with a
/// printed reason) when it was never built.
fn find_real_worker() -> Option<std::path::PathBuf> {
	if let Ok(p) = std::env::var("QUERCUS_WORKER_BIN") {
		let p = std::path::PathBuf::from(p);
		if p.exists() {
			return Some(p);
		}
	}
	let exe = std::env::current_exe().ok()?;
	let candidate = exe
		.parent()?
		.parent()?
		.join(format!("quercus-worker{}", std::env::consts::EXE_SUFFIX));
	candidate.exists().then_some(candidate)
}

#[test]
fn slot_bytes_for_format_matrix() {
	assert_eq!(slot_bytes_for(2, 2, SLOT_FORMAT_BGRA8), 16);
	// U8: 1 byte/channel * 4 channels.
	assert_eq!(slot_bytes_for(2, 2, 0), 16);
	// U10 reports 4 bytes/channel (the packed RGBA10A2 word), so the
	// slot math is 4 * channels bytes per pixel.
	assert_eq!(slot_bytes_for(2, 2, 1), 64);
	assert_eq!(slot_bytes_for(2, 2, 2), 32); // U16
	assert_eq!(slot_bytes_for(2, 2, 3), 32); // F16
	assert_eq!(slot_bytes_for(2, 2, 4), 64); // F32
	assert_eq!(slot_bytes_for(2, 2, 9), 64); // unknown -> F32
										  // Negative dimensions clamp to zero pixels.
	assert_eq!(slot_bytes_for(-3, 5, 0), 0);
	assert_eq!(slot_bytes_for(3, -5, 0), 0);
}

#[test]
fn bgra8_to_f32_rgba_converts_and_ignores_tail() {
	let out = bgra8_to_f32_rgba(&[255, 128, 0, 255, 10]);
	assert_eq!(
		out.len(),
		4,
		"trailing byte without a full pixel is dropped"
	);
	assert_eq!(out[0], 0.0);
	assert!((out[1] - 128.0 / 255.0).abs() < 1e-6);
	assert_eq!(out[2], 1.0);
	assert_eq!(out[3], 1.0);
	assert!(bgra8_to_f32_rgba(&[]).is_empty());
}

#[test]
fn defaults_policies_edge_inputs() {
	// slot_bytes 0 still yields a sane count (the max(1) guards the divide).
	assert_eq!(default_slots_for_bytes(0, 8), 8);
	// Shrinking below the floor clamps to 2.
	assert_eq!(default_slots_for_bytes(8_300_000, 1), 2);
	// A null divider falls back to a per-worker count >= 1.
	assert!(default_worker_count(0, 0) >= 1);
	assert_eq!(default_batch_size(0, 0), 1);
}

#[test]
fn shm_view_refs_samples_and_debug() {
	let key = shm_key("refs");
	let view = ShmRegionView::create(&key, 2, 64).expect("shm");
	let slot = 0u32;
	let samples_in = [1.0f32, -2.5, 0.0];
	let bytes: Vec<u8> = samples_in.iter().flat_map(|v| v.to_le_bytes()).collect();
	unsafe {
		let pool = view.pool();
		let dst = std::slice::from_raw_parts_mut(pool.slot_data(slot), bytes.len());
		dst.copy_from_slice(&bytes);
		(*pool.meta(slot)).id = 7;
	}
	assert_eq!(view.key(), key);
	assert_eq!(view.slot_count(), 2);
	assert_eq!(view.slot_data_bytes(), 64);
	assert_eq!(view.slot_bytes(slot).len(), 64);
	assert_eq!(view.meta_copy(slot).id, 7);

	let meta = ShmFrameMeta {
		id: 7,
		time_num: 1,
		time_den: 25,
		width: 2,
		height: 2,
		format: crate::ipc::SLOT_FORMAT_AUDIO_F32,
		channel_count: 2,
		linesize: 8,
		data_size: bytes.len() as i32,
		colorspace: "linear".to_string(),
	};
	let audio = ShmAudioRef {
		worker: 0,
		slot,
		meta: meta.clone(),
		shm: view.clone(),
		sample_rate: 48000,
		channel_layout: 0x3,
		channel_count: 2,
	};
	assert_eq!(audio.samples(), samples_in.to_vec());
	let decoded = audio.to_audio_samples();
	assert_eq!(decoded.samples, samples_in.to_vec());
	assert_eq!(decoded.sample_rate, 48000);
	assert_eq!(decoded.channel_layout, 0x3);
	assert_eq!(decoded.channel_count, 2);
	assert!(format!("{audio:?}").contains("ShmAudioRef"));
	let frame = audio.frame_ref();
	assert_eq!(frame.worker, 0);
	assert_eq!(frame.slot, slot);
	assert_eq!(frame.meta.id, 7);
	assert!(format!("{frame:?}").contains("ShmFrameRef"));

	// A bogus (oversized / negative) data size yields no samples.
	let mut clipped = audio.clone();
	clipped.meta.data_size = i32::MAX;
	assert!(clipped.samples().is_empty());
	clipped.meta.data_size = -1;
	assert!(clipped.samples().is_empty());
}

#[test]
fn shm_region_create_invalid_key_errors() {
	// A NUL in the key makes every shm_open attempt fail: the create
	// helper unlinks and retries once, then surfaces the error.
	let err = ShmRegionView::create("bad\0key", 1, 64)
		.err()
		.expect("a NUL key can never create a segment");
	assert!(format!("{err}").contains("create shm segment"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_drm_vram_default_query_smoke() {
	// The real sysfs walk: no assertion on the result (GPU-dependent),
	// just exercise the path that forwards to the fixture-testable walk.
	let _ = linux_drm_vram_bytes();
	assert!(linux_drm_vram_bytes_from(std::path::Path::new("/nonexistent")).is_none());
}

#[test]
fn worker_count_for_size_with_hwaccel_disabled() {
	let _lock = pool_test_lock();
	let _hwaccel = EnvRestore::set("QUERCUS_HWACCEL", "0");
	assert!(!hwdecode_available());
	// Hardware decoding off: the GPU-vram policy never engages.
	let base = default_worker_count(2, 128);
	assert_eq!(worker_count_for_size(2, 128, (64, 64), 30), base);
	// Re-enabling leaves the CPU/RAM policy as the upper bound.
	std::env::remove_var("QUERCUS_HWACCEL");
	let with_gpu = worker_count_for_size(2, 128, (64, 64), 30);
	assert!(
		with_gpu >= 1 && with_gpu <= base,
		"the GPU-vram bound only caps the CPU/RAM policy ({with_gpu} vs {base})"
	);
}

#[test]
fn cancel_frame_pending_claimed_and_unknown() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	let key = FrameKey {
		sequence: 9,
		frame: 3,
		version: 1,
	};
	// Unknown key: the scheduler reports false and nothing fires.
	dispatcher.cancel_frame(&key);
	let results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	let results2 = results.clone();
	{
		let mut inner = lock(&dispatcher.inner);
		inner.scheduler.submit(FrameRequest {
			key,
			priority: crate::scheduler::FramePriority::Seek,
			distance: 0,
			payload: 1,
			slot_bytes: 64,
		});
		inner.tickets.insert(
			1,
			PendingTicket {
				key,
				params: Arc::new(video_params(3)),
				audio: None,
				done: Some(Box::new(move |result| {
					results2
						.lock()
						.unwrap_or_else(|e| e.into_inner())
						.push(result);
				})),
			},
		);
	}
	// The cancel locks the dispatcher internally: never call it while
	// holding `dispatcher.inner` (that deadlocks the test).
	dispatcher.cancel_frame(&key);
	{
		let inner = lock(&dispatcher.inner);
		assert!(inner.tickets.is_empty());
	}
	assert_eq!(results.lock().unwrap().len(), 1);
	assert!(results.lock().unwrap()[0].is_err());

	// Claimed (in-flight) cancel removes the claim and fires once.
	let key2 = FrameKey {
		sequence: 9,
		frame: 4,
		version: 1,
	};
	let results2: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	let sink = results2.clone();
	{
		let mut inner = lock(&dispatcher.inner);
		inner.scheduler.submit(FrameRequest {
			key: key2,
			priority: crate::scheduler::FramePriority::Playback,
			distance: 0,
			payload: 2,
			slot_bytes: 64,
		});
		let claimed = inner.scheduler.claim_batch(0, 2, 64).expect("claimed");
		assert_eq!(claimed.frames.len(), 1);
		inner.tickets.insert(
			2,
			PendingTicket {
				key: key2,
				params: Arc::new(video_params(4)),
				audio: None,
				done: Some(Box::new(move |result| {
					sink.lock().unwrap_or_else(|e| e.into_inner()).push(result)
				})),
			},
		);
	}
	dispatcher.cancel_frame(&key2);
	assert_eq!(results2.lock().unwrap().len(), 1);
	assert!(results2.lock().unwrap()[0].is_err());
	assert_eq!(dispatcher.worker_count(), 1);
}

#[test]
fn release_frame_stale_double_and_missing_worker() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	let key = shm_key("release");
	let shm = ShmRegionView::create(&key, 2, 64).expect("shm");
	{
		let mut inner = lock(&dispatcher.inner);
		let mut handle = WorkerHandle::shell(0, shm.clone(), 2, 64);
		handle.state = WorkerState::Alive;
		inner.workers.push(handle);
	}
	let meta = shm.meta_copy(0);
	// Unknown worker index: ignored.
	let unknown = crate::procpool::ShmFrameRef {
		worker: 42,
		slot: 0,
		meta: meta.clone(),
		shm: shm.clone(),
	};
	dispatcher.release_frame(&unknown);
	// Stale ref (same worker index, different segment): ignored.
	let other_key = shm_key("release-other");
	let other = ShmRegionView::create(&other_key, 1, 64).expect("shm");
	let stale = crate::procpool::ShmFrameRef {
		worker: 0,
		slot: 0,
		meta: meta.clone(),
		shm: other,
	};
	dispatcher.release_frame(&stale);
	{
		let inner = lock(&dispatcher.inner);
		assert!(inner.workers[0].held.is_empty());
	}
	// A held slot releases once; the second release is a no-op.
	{
		let mut inner = lock(&dispatcher.inner);
		inner.workers[0].held.insert(0);
		inner.workers[0].free_slots.clear();
	}
	let frame = crate::procpool::ShmFrameRef {
		worker: 0,
		slot: 0,
		meta,
		shm,
	};
	dispatcher.release_frame(&frame);
	{
		let inner = lock(&dispatcher.inner);
		assert_eq!(
			inner.workers[0]
				.free_slots
				.iter()
				.filter(|&&s| s == 0)
				.count(),
			1
		);
	}
	dispatcher.release_frame(&frame);
	{
		let inner = lock(&dispatcher.inner);
		assert_eq!(
			inner.workers[0]
				.free_slots
				.iter()
				.filter(|&&s| s == 0)
				.count(),
			1,
			"double release is ignored"
		);
	}
}

#[test]
fn audio_ref_release_delegates_and_cancels_sequence() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 1)).expect("dispatcher");
	// The audio release delegate reaches release_frame (unknown frame
	// index -> early return, no panic).
	let key = shm_key("audio-release");
	let shm = ShmRegionView::create(&key, 1, 64).expect("shm");
	let audio = ShmAudioRef {
		worker: 99,
		slot: 0,
		meta: shm.meta_copy(0),
		shm,
		sample_rate: 48000,
		channel_layout: 0x3,
		channel_count: 2,
	};
	dispatcher.release_audio_frame(&audio);

	// cancel_preview_sequence drops pending + claimed requests and
	// fires their completions.
	let results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	for (i, frame) in [10i64, 11].into_iter().enumerate() {
		let key = FrameKey {
			sequence: 77,
			frame,
			version: 0,
		};
		let sink = results.clone();
		let mut inner = lock(&dispatcher.inner);
		inner.scheduler.submit(FrameRequest {
			key,
			priority: crate::scheduler::FramePriority::Playback,
			distance: 0,
			payload: i as i64,
			slot_bytes: 64,
		});
		inner.tickets.insert(
			i as i64,
			PendingTicket {
				key,
				params: Arc::new(video_params(frame)),
				audio: None,
				done: Some(Box::new(move |result| {
					sink.lock().unwrap_or_else(|e| e.into_inner()).push(result)
				})),
			},
		);
	}
	{
		let mut inner = lock(&dispatcher.inner);
		// The Playback reserve claims one now; the other stays pending,
		// so cancel_sequence covers both the pending and claimed arms.
		let claimed = inner.scheduler.claim_batch(0, 2, 64).expect("claimed");
		assert_eq!(claimed.frames.len(), 1);
	}
	dispatcher.cancel_preview_sequence(77);
	assert_eq!(results.lock().unwrap().len(), 2);
	assert!(results.lock().unwrap().iter().all(|r| r.is_err()));
	let _ = shm;
}

#[test]
fn job_dispatch_trait_delegations() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 1)).expect("dispatcher");
	let raw: &ProcessDispatcher = &dispatcher;
	JobDispatch::poll(raw);
	assert_eq!(
		JobDispatch::preview_window_capacity(raw),
		Some(dispatcher.preview_window_capacity())
	);
	JobDispatch::cancel_preview_frame(raw, 1, 2, 3);
	JobDispatch::set_graph_snapshot(raw, Some("/nonexistent/graph.xml".into()));
	JobDispatch::set_graph_snapshot(raw, None);
	let key = shm_key("trait-release");
	let shm = ShmRegionView::create(&key, 1, 64).expect("shm");
	let frame = ShmFrameRef {
		worker: 99,
		slot: 0,
		meta: shm.meta_copy(0),
		shm: shm.clone(),
	};
	// Unknown worker: both release delegates take the early return.
	JobDispatch::release_frame(raw, &frame);
	let audio = ShmAudioRef {
		worker: 99,
		slot: 0,
		meta: shm.meta_copy(0),
		shm,
		sample_rate: 48000,
		channel_layout: 0x3,
		channel_count: 2,
	};
	JobDispatch::release_audio_frame(raw, &audio);
}

#[test]
fn on_line_protocol_dispatch_and_failures() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	let mut fired: Vec<(Completion, TicketResult)> = Vec::new();
	{
		let mut inner = lock(&dispatcher.inner);
		let key = shm_key("on-line");
		let shm = ShmRegionView::create(&key, 2, 64).expect("shm");
		let mut handle = WorkerHandle::shell(7, shm, 2, 64);
		handle.state = WorkerState::Starting;
		inner.workers.push(handle);

		// Malformed / non-object lines and unknown workers return early.
		dispatcher.on_line(&mut inner, 0, "not json", &mut fired);
		dispatcher.on_line(&mut inner, 0, "[1,2]", &mut fired);
		dispatcher.on_line(&mut inner, 99, r#"{"type":"hello_caps"}"#, &mut fired);

		// A handshake on a handle without stdin fails to reply -> Dead.
		dispatcher.on_line(
			&mut inner,
			0,
			r#"{"type":"handshake","protocol_version":1}"#,
			&mut fired,
		);
		assert!(inner.workers[0].startup_seen);
		assert_eq!(inner.workers[0].state, WorkerState::Dead);

		// hello_caps marks the worker alive; the pending load_graph send
		// then fails (no stdin) and kills it again.
		inner.config.graph_snapshot = Some("/nonexistent/graph.xml".into());
		inner.workers[0].state = WorkerState::Starting;
		inner.workers[0].graph_sent = false;
		dispatcher.on_line(
			&mut inner,
			0,
			r#"{"type":"hello_caps","protocol_version":1,"formats":[4],"max_slot_bytes":4096}"#,
			&mut fired,
		);
		assert!(inner.workers[0].caps.is_some());
		assert!(!inner.workers[0].reconfiguring);
		assert!(inner.workers[0].graph_sent);
		assert_eq!(inner.workers[0].state, WorkerState::Dead);

		// batch_accepted increments the metric (no reply needed).
		dispatcher.on_line(
			&mut inner,
			0,
			r#"{"type":"batch_accepted","batch_id":1,"tickets":[1,2]}"#,
			&mut fired,
		);
		assert_eq!(inner.workers[0].accepted_batches, 1);

		// A session-level error on a Starting worker recycles it.
		inner.workers[0].state = WorkerState::Starting;
		dispatcher.on_line(
			&mut inner,
			0,
			r#"{"type":"error","message":"shm attach failed"}"#,
			&mut fired,
		);
		assert_eq!(inner.workers[0].state, WorkerState::Dead);

		// An error WITH a ticket routes to on_frame_failed.
		dispatcher.on_line(
			&mut inner,
			0,
			r#"{"type":"error","ticket":5,"message":"boom"}"#,
			&mut fired,
		);
		// Unknown type and malformed known messages are ignored.
		dispatcher.on_line(&mut inner, 0, r#"{"type":"something_else"}"#, &mut fired);
		dispatcher.on_line(
			&mut inner,
			0,
			r#"{"type":"hello_caps","protocol_version":"x"}"#,
			&mut fired,
		);
		dispatcher.on_line(
			&mut inner,
			0,
			r#"{"type":"batch_accepted","batch_id":"x"}"#,
			&mut fired,
		);
		dispatcher.on_line(
			&mut inner,
			0,
			r#"{"type":"frame_ready","ticket":"x"}"#,
			&mut fired,
		);
		dispatcher.on_line(&mut inner, 0, r#"{"type":"frame_failed"}"#, &mut fired);
		// plugin_progress forwards only when a callback is registered.
		set_plugin_progress_cb(None);
		dispatcher.on_line(
			&mut inner,
			0,
			r#"{"type":"plugin_progress","label":"a","message":"b","fraction":0.5}"#,
			&mut fired,
		);
	}
	assert!(fired.is_empty());
}

#[test]
fn on_frame_ready_variants() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 8)).expect("dispatcher");
	let key = shm_key("frame-ready");
	let shm = ShmRegionView::create(&key, 8, 64).expect("shm");
	{
		let mut inner = lock(&dispatcher.inner);
		let mut handle = WorkerHandle::shell(0, shm.clone(), 8, 64);
		handle.state = WorkerState::Alive;
		inner.workers.push(handle);
	}
	let mut fired: Vec<(Completion, TicketResult)> = Vec::new();

	// Unknown ticket: a late / duplicate frame_ready is ignored (the
	// debug trace covers its log line).
	{
		let mut inner = lock(&dispatcher.inner);
		let env = EnvRestore::set("QUERCUS_DEBUG_DISPATCH", "1");
		dispatcher.on_frame_ready(&mut inner, 0, 404, 0, &mut fired);
		drop(env);
	}
	assert!(fired.is_empty());

	// Video ticket: the published slot completes as ShmFrame.
	let video_slot = publish_slot(&shm, 42);
	let video_results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	{
		let mut inner = lock(&dispatcher.inner);
		inner.workers[0].outstanding.insert(11, video_slot);
		let sink = video_results.clone();
		inner.tickets.insert(
			11,
			PendingTicket {
				key: FrameKey {
					sequence: 1,
					frame: 0,
					version: 0,
				},
				params: Arc::new(video_params(0)),
				audio: None,
				done: Some(Box::new(move |result| {
					sink.lock().unwrap_or_else(|e| e.into_inner()).push(result)
				})),
			},
		);
		dispatcher.on_frame_ready(&mut inner, 0, 11, video_slot as i32, &mut fired);
	}
	assert_eq!(fired.len(), 1);
	{
		let (done, result) = fired.pop().unwrap();
		done(result);
	}
	let video_got = video_results.lock().unwrap();
	assert!(
		matches!(&video_got[0], Ok(TicketPayload::ShmFrame(_))),
		"expected ShmFrame, got {:?}",
		video_got[0]
	);
	if let Ok(TicketPayload::ShmFrame(frame)) = &video_got[0] {
		assert_eq!(frame.meta.id, 42);
		assert_eq!(frame.worker, 0);
		assert_eq!(frame.slot, video_slot);
	}

	// Audio ticket: the same slot hand-off yields ShmAudio.
	let audio_slot = publish_slot(&shm, 43);
	let audio_results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	{
		let mut inner = lock(&dispatcher.inner);
		inner.workers[0].outstanding.insert(12, audio_slot);
		let sink = audio_results.clone();
		inner.tickets.insert(
			12,
			PendingTicket {
				key: FrameKey {
					sequence: 2,
					frame: 0,
					version: 0,
				},
				params: Arc::new(video_params(0)),
				audio: Some(Arc::new(AudioTicketParams {
					viewer: 1,
					range: quercus_core::TimeRange::new(
						quercus_core::Rational::new(0, 1),
						quercus_core::Rational::new(1, 48),
					),
					sample_rate: 48000,
					channel_layout: 0x3,
					montage: Vec::new(),
					bus_effects: Vec::new(),
				})),
				done: Some(Box::new(move |result| {
					sink.lock().unwrap_or_else(|e| e.into_inner()).push(result)
				})),
			},
		);
		dispatcher.on_frame_ready(&mut inner, 0, 12, audio_slot as i32, &mut fired);
	}
	assert_eq!(fired.len(), 1);
	{
		let (done, result) = fired.pop().unwrap();
		done(result);
	}
	let audio_got = audio_results.lock().unwrap();
	assert!(
		matches!(&audio_got[0], Ok(TicketPayload::ShmAudio(_))),
		"expected ShmAudio, got {:?}",
		audio_got[0]
	);
	if let Ok(TicketPayload::ShmAudio(audio)) = &audio_got[0] {
		assert_eq!(audio.meta.id, 43);
		assert_eq!(audio.sample_rate, 48000);
		assert_eq!(audio.channel_count, 2);
	}

	// Cancelled in flight (completion taken): the slot recycles now.
	let cancelled_slot = publish_slot(&shm, 44);
	{
		let mut inner = lock(&dispatcher.inner);
		inner.workers[0].outstanding.insert(13, cancelled_slot);
		inner.tickets.insert(
			13,
			PendingTicket {
				key: FrameKey {
					sequence: 3,
					frame: 0,
					version: 0,
				},
				params: Arc::new(video_params(0)),
				audio: None,
				done: None,
			},
		);
		let before = inner.workers[0].free_slots.len();
		dispatcher.on_frame_ready(&mut inner, 0, 13, cancelled_slot as i32, &mut fired);
		assert_eq!(inner.workers[0].free_slots.len(), before + 1);
		assert!(!inner.workers[0].held.contains(&cancelled_slot));
	}
	// An outstanding entry with no ticket row recycles too.
	let orphan_slot = publish_slot(&shm, 45);
	{
		let mut inner = lock(&dispatcher.inner);
		inner.workers[0].outstanding.insert(14, orphan_slot);
		dispatcher.on_frame_ready(&mut inner, 0, 14, orphan_slot as i32, &mut fired);
	}
	assert!(fired.is_empty());

	// Ready-ring out of sync: the reported slot wins, the mismatch is
	// logged and the completion still lands.
	let published = publish_slot(&shm, 46);
	let reported = if published == 0 { 1 } else { 0 };
	unsafe {
		(*shm.pool().meta(reported)).id = 46;
	}
	let results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	{
		let mut inner = lock(&dispatcher.inner);
		inner.workers[0].outstanding.insert(15, reported);
		let sink = results.clone();
		inner.tickets.insert(
			15,
			PendingTicket {
				key: FrameKey {
					sequence: 4,
					frame: 0,
					version: 0,
				},
				params: Arc::new(video_params(0)),
				audio: None,
				done: Some(Box::new(move |result| {
					sink.lock().unwrap_or_else(|e| e.into_inner()).push(result)
				})),
			},
		);
		dispatcher.on_frame_ready(&mut inner, 0, 15, reported as i32, &mut fired);
	}
	assert_eq!(fired.len(), 1);
	{
		let (done, result) = fired.pop().unwrap();
		done(result);
	}
	assert!(matches!(
		&results.lock().unwrap()[0],
		Ok(TicketPayload::ShmFrame(frame)) if frame.meta.id == 46
	));
}

#[test]
fn on_frame_failed_paths() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	let mut fired: Vec<(Completion, TicketResult)> = Vec::new();
	let results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("frame-failed"), 2, 64).expect("shm");
		let mut handle = WorkerHandle::shell(0, shm, 2, 64);
		handle.state = WorkerState::Alive;
		inner.workers.push(handle);

		// Unknown worker -> early return (with the debug trace on).
		let env = EnvRestore::set("QUERCUS_DEBUG_DISPATCH", "1");
		dispatcher.on_frame_failed(&mut inner, 99, 1, "x", &mut fired);
		drop(env);
		// Known worker, unknown ticket -> early return.
		dispatcher.on_frame_failed(&mut inner, 0, 1, "x", &mut fired);
		// recycle_slot with an unknown worker is a no-op.
		dispatcher.recycle_slot(&mut inner, 99, 0);

		// A known outstanding ticket fails: slot recycled, completion Err.
		inner.workers[0].outstanding.insert(3, 1);
		inner.workers[0].held.insert(1);
		let sink = results.clone();
		inner.tickets.insert(
			3,
			PendingTicket {
				key: FrameKey {
					sequence: 2,
					frame: 0,
					version: 0,
				},
				params: Arc::new(video_params(0)),
				audio: None,
				done: Some(Box::new(move |result| {
					sink.lock().unwrap_or_else(|e| e.into_inner()).push(result)
				})),
			},
		);
		dispatcher.on_frame_failed(&mut inner, 0, 3, "boom", &mut fired);
		assert!(inner.workers[0].held.is_empty());
		assert!(inner.workers[0].free_slots.contains(&1));
		assert!(inner.tickets.is_empty());
	}
	assert_eq!(fired.len(), 1);
	{
		let (done, result) = fired.pop().unwrap();
		done(result);
	}
	assert!(results.lock().unwrap()[0].is_err());
}

#[test]
fn rebuild_segment_failure_paths() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("rebuild"), 2, 128).expect("shm");
		let mut handle = WorkerHandle::shell(0, shm, 2, 128);
		handle.state = WorkerState::Alive;
		inner.workers.push(handle);

		// A valid small grow with no stdin: the re-attach handshake
		// send fails and the worker is marked Dead.
		dispatcher.rebuild_segment(&mut inner, 0, 128).unwrap();
		assert!(inner.workers[0].reconfiguring);
		assert_eq!(inner.workers[0].slot_bytes, 128);
		assert_eq!(inner.workers[0].state, WorkerState::Dead);

		// A hostile slot size makes the segment create fail: the grow
		// error is logged and the handle keeps its old geometry.
		inner.workers[0].state = WorkerState::Alive;
		inner.workers[0].outstanding.clear();
		inner.workers[0].free_slots = (0..2).collect();
		inner.scheduler.submit(FrameRequest {
			key: FrameKey {
				sequence: 3,
				frame: 0,
				version: 0,
			},
			priority: crate::scheduler::FramePriority::Seek,
			distance: 0,
			payload: 1,
			slot_bytes: 1 << 50,
		});
		dispatcher.dispatch_to(&mut inner, 0);
		assert_eq!(inner.workers[0].slot_bytes, 128);
		assert_eq!(inner.workers[0].state, WorkerState::Alive);
	}
}

#[test]
fn dispatch_to_grow_failure_and_starvation_log() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("starvation"), 2, 128).expect("shm");
		let mut handle = WorkerHandle::shell(0, shm, 2, 128);
		handle.state = WorkerState::Alive;
		inner.workers.push(handle);

		// Grow failure: a pending request far larger than any segment
		// fails the rebuild and dispatch stops for this pump.
		let huge_key = FrameKey {
			sequence: 100,
			frame: 0,
			version: 0,
		};
		inner.scheduler.submit(FrameRequest {
			key: huge_key,
			priority: crate::scheduler::FramePriority::Seek,
			distance: 0,
			payload: 1,
			slot_bytes: 1 << 50,
		});
		dispatcher.dispatch_to(&mut inner, 0);
		assert_eq!(inner.scheduler.pending_len(), 1);
		inner.scheduler.cancel_sequence(100);

		// Starvation: outstanding work blocks the grow; the pending
		// request is too large for the current slots -> claim reports
		// None, and the debug flag logs why.
		inner.workers[0].outstanding.insert(999, 0);
		inner.workers[0].free_slots = (0..2).collect();
		inner.scheduler.submit(FrameRequest {
			key: FrameKey {
				sequence: 101,
				frame: 0,
				version: 0,
			},
			priority: crate::scheduler::FramePriority::Seek,
			distance: 0,
			payload: 2,
			slot_bytes: 4096,
		});
		let env = EnvRestore::set("QUERCUS_DEBUG_DISPATCH", "1");
		dispatcher.dispatch_to(&mut inner, 0);
		drop(env);
		assert_eq!(inner.scheduler.pending_len(), 1);
		assert_eq!(inner.workers[0].outstanding.len(), 1);
		assert_eq!(inner.workers[0].free_slots.len(), 2);
	}
}

#[test]
fn dispatch_to_skips_claimed_request_without_ticket() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("no-ticket"), 2, 128).expect("shm");
		let mut handle = WorkerHandle::shell(0, shm, 2, 128);
		handle.state = WorkerState::Alive;
		inner.workers.push(handle);
		inner.scheduler.submit(FrameRequest {
			key: FrameKey {
				sequence: 4,
				frame: 0,
				version: 0,
			},
			priority: crate::scheduler::FramePriority::Seek,
			distance: 0,
			payload: 12345,
			slot_bytes: 64,
		});
		// The claimed request has no ticket entry: the slot stays
		// assigned and the dispatch loop continues cleanly.
		dispatcher.dispatch_to(&mut inner, 0);
		assert_eq!(inner.scheduler.pending_len(), 0);
		assert_eq!(inner.workers[0].outstanding.len(), 1);
		assert_eq!(inner.workers[0].free_slots.len(), 1);
	}
}

#[cfg(unix)]
#[test]
fn dispatch_to_video_audio_and_control_line_sends() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	let mut child = std::process::Command::new("/bin/cat")
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.spawn()
		.expect("/bin/cat");
	let stdin = child.stdin.take();
	let mut fired: Vec<(Completion, TicketResult)> = Vec::new();
	let env = EnvRestore::set("QUERCUS_DEBUG_DISPATCH", "1");
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("cat-sends"), 2, 256).expect("shm");
		let mut handle = WorkerHandle::shell(0, shm, 2, 256);
		handle.state = WorkerState::Alive;
		handle.stdin = stdin;
		handle.child = Some(child);
		inner.workers.push(handle);

		// Control-plane writes succeed: handshake reply, then the
		// hello_caps-triggered load_graph.
		dispatcher.on_line(&mut inner, 0, r#"{"type":"handshake"}"#, &mut fired);
		inner.config.graph_snapshot = Some("/nonexistent/graph.xml".into());
		inner.workers[0].graph_sent = false;
		dispatcher.on_line(
			&mut inner,
			0,
			r#"{"type":"hello_caps","protocol_version":1,"formats":[4],"max_slot_bytes":4096}"#,
			&mut fired,
		);
		assert!(inner.workers[0].graph_sent);
		assert_eq!(inner.workers[0].state, WorkerState::Alive);

		// A mixed video+audio batch is written as two messages (the
		// debug log runs with the env var set).
		inner.tickets.insert(
			1,
			PendingTicket {
				key: FrameKey {
					sequence: 5,
					frame: 0,
					version: 0,
				},
				params: Arc::new(video_params(0)),
				audio: None,
				done: None,
			},
		);
		inner.tickets.insert(
			2,
			PendingTicket {
				key: FrameKey {
					sequence: 5,
					frame: 1,
					version: 0,
				},
				params: Arc::new(video_params(1)),
				audio: Some(Arc::new(AudioTicketParams {
					viewer: 1,
					range: quercus_core::TimeRange::new(
						quercus_core::Rational::new(0, 1),
						quercus_core::Rational::new(1, 480),
					),
					sample_rate: 48000,
					channel_layout: 0x3,
					montage: Vec::new(),
					bus_effects: Vec::new(),
				})),
				done: None,
			},
		);
		inner.scheduler.submit(FrameRequest {
			key: FrameKey {
				sequence: 5,
				frame: 0,
				version: 0,
			},
			priority: crate::scheduler::FramePriority::Seek,
			distance: 0,
			payload: 1,
			slot_bytes: 64,
		});
		inner.scheduler.submit(FrameRequest {
			key: FrameKey {
				sequence: 5,
				frame: 1,
				version: 0,
			},
			priority: crate::scheduler::FramePriority::Seek,
			distance: 0,
			payload: 2,
			slot_bytes: 64,
		});
		dispatcher.dispatch_to(&mut inner, 0);
		assert_eq!(inner.workers[0].outstanding.len(), 2);
		assert_eq!(inner.workers[0].free_slots.len(), 0);
		// The scheduler must know about the synthetic workers below.
		inner.scheduler.set_worker_count(3);

		// Finish here: `post`/`shutdown` lock the dispatcher
		// internally, so they run outside the guard below.
	}
	dispatcher.set_graph_snapshot(Some("/nonexistent/graph2.xml".into()));
	dispatcher.set_graph_snapshot(None);
	let results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	assert!(dispatcher.post(test_job(8, 0, None, &results)));
	drop(env);
	{
		let mut inner = lock(&dispatcher.inner);
		if let Some(handle) = inner.workers.get_mut(0) {
			if let Some(mut c) = handle.child.take() {
				let _ = c.kill();
				let _ = c.wait();
			}
			handle.stdin = None;
		}
	}
	dispatcher.shutdown();
}

/// A missing stdin on a dispatchable worker fails the send and marks
/// the worker dead (recycled).
#[test]
fn dispatch_to_missing_stdin_recycles_the_worker() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	let key = FrameKey {
		sequence: 11,
		frame: 0,
		version: 0,
	};
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("no-stdin"), 2, 256).expect("shm");
		let mut handle = WorkerHandle::shell(0, shm, 2, 256);
		handle.state = WorkerState::Alive;
		// No stdin: the send must fail and recycle the worker.
		inner.workers.push(handle);
		inner.tickets.insert(
			9,
			PendingTicket {
				key,
				params: Arc::new(video_params(0)),
				audio: None,
				done: None,
			},
		);
		inner.scheduler.submit(FrameRequest {
			key,
			priority: crate::scheduler::FramePriority::Seek,
			distance: 0,
			payload: 9,
			slot_bytes: 64,
		});
		dispatcher.dispatch_to(&mut inner, 0);
		assert_eq!(inner.workers[0].state, WorkerState::Dead);
	}
}

#[test]
fn post_replace_inflight_oversized_and_shutdown() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	let results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));

	// Replaced: a second post for the same key supersedes the first.
	let mut first = test_job(1, 0, None, &results);
	first.schedule.frame = Some(0);
	assert!(dispatcher.post(first));
	let mut second = test_job(1, 0, None, &results);
	second.schedule.frame = Some(0);
	assert!(dispatcher.post(second));
	{
		let got = results.lock().unwrap();
		assert_eq!(got.len(), 1, "the replaced ticket fires once");
		assert!(got[0].is_err());
	}
	results.lock().unwrap().clear();

	// InFlight: the key is already claimed by a worker.
	let key = FrameKey {
		sequence: 5,
		frame: 0,
		version: 0,
	};
	{
		let mut inner = lock(&dispatcher.inner);
		inner.scheduler.submit(FrameRequest {
			key,
			priority: crate::scheduler::FramePriority::Seek,
			distance: 0,
			payload: 7,
			slot_bytes: 64,
		});
		let claimed = inner.scheduler.claim_batch(0, 2, 64).expect("claim");
		assert_eq!(claimed.frames.len(), 1);
	}
	let mut inflight = test_job(5, 0, None, &results);
	inflight.schedule.frame = Some(0);
	assert!(dispatcher.post(inflight));
	{
		let got = results.lock().unwrap();
		assert_eq!(got.len(), 1, "the in-flight ticket is cancelled");
		assert!(got[0].is_err());
	}
	results.lock().unwrap().clear();

	// Oversized audio is refused (the arena falls back inline).
	let big = Arc::new(AudioTicketParams {
		viewer: 1,
		range: quercus_core::TimeRange::new(
			quercus_core::Rational::new(0, 1),
			quercus_core::Rational::new(175, 1),
		),
		sample_rate: 48000,
		channel_layout: 0x3,
		montage: Vec::new(),
		bus_effects: Vec::new(),
	});
	assert!(!dispatcher.post(test_job(6, 0, Some(big), &results)));
	// An invalid (zero-length) audio range is refused too.
	let zero = Arc::new(AudioTicketParams {
		viewer: 1,
		range: quercus_core::TimeRange::new(
			quercus_core::Rational::new(0, 1),
			quercus_core::Rational::new(0, 1),
		),
		sample_rate: 48000,
		channel_layout: 0x3,
		montage: Vec::new(),
		bus_effects: Vec::new(),
	});
	assert!(!dispatcher.post(test_job(6, 0, Some(zero), &results)));

	// Debug post logging with a synthetic (Starting) worker present.
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("post-debug"), 2, 64).expect("shm");
		inner.workers.push(WorkerHandle::shell(0, shm, 2, 64));
	}
	let env = EnvRestore::set("QUERCUS_DEBUG_DISPATCH", "1");
	assert!(dispatcher.post(test_job(9, 0, None, &results)));
	drop(env);

	// After shutdown posts are refused and open tickets are cancelled.
	dispatcher.shutdown();
	assert!(!dispatcher.post(test_job(10, 0, None, &results)));
	let got = results.lock().unwrap();
	assert!(
		!got.is_empty() && got.iter().all(|r| r.is_err()),
		"open tickets cancelled at shutdown"
	);
}

#[cfg(unix)]
#[test]
fn start_failure_restart_budget_and_accessors() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	// Not started: set_target_workers is a no-op.
	dispatcher.set_target_workers(4);
	assert_eq!(dispatcher.worker_count(), 1);
	assert_eq!(dispatcher.slots_per_worker(), 2);
	assert_eq!(dispatcher.slot_bytes(), 16 * 16 * 4);
	assert_eq!(dispatcher.slot_format(), SLOT_FORMAT_BGRA8);
	assert!(!dispatcher.is_alive(0));
	assert_eq!(dispatcher.restarts_of(0), 0);
	assert_eq!(dispatcher.accepted_batches_of(0), 0);
	assert!(dispatcher.shm_of(0).is_none());
	// Nobody alive: the window reports the configured pool.
	assert_eq!(dispatcher.preview_window_capacity(), 1);

	// `/bin/true` exits immediately: the restart budget exhausts and
	// start() fails permanently.
	assert!(dispatcher.start().is_err());
	assert!(!dispatcher.is_alive(0));
	assert!(dispatcher.restarts_of(0) > MAX_RESTARTS);
	assert_eq!(dispatcher.accepted_batches_of(0), 0);
	assert_eq!(dispatcher.accepted_batches_of(99), 0);
	assert!(dispatcher.shm_of(0).is_some());
	assert!(dispatcher.shm_of(99).is_none());

	// A second start is rejected; a same-target resize is a no-op.
	assert!(dispatcher.start().is_err());
	dispatcher.set_target_workers(1);

	// poll() after shutdown is a no-op; shutdown is idempotent.
	dispatcher.shutdown();
	dispatcher.poll();
	dispatcher.shutdown();
	let results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	assert!(!dispatcher.post(test_job(1, 0, None, &results)));
	assert!(results.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn resize_throttle_grow_failure_and_shrink_drain() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	assert!(dispatcher.start().is_err(), "/bin/true never handshakes");
	{
		let mut inner = lock(&dispatcher.inner);
		inner.bin = PathBuf::from("/nonexistent/quercus-worker-for-resize-test");
		inner.last_resize_at = None;
	}
	// Grow: the spawn failure is logged and the grow loop breaks.
	dispatcher.set_target_workers(2);
	assert_eq!(dispatcher.worker_count(), 2);
	// A request inside the throttle window only stores the target.
	{
		let mut inner = lock(&dispatcher.inner);
		inner.last_resize_at = Some(Instant::now());
	}
	dispatcher.set_target_workers(1);
	{
		let mut inner = lock(&dispatcher.inner);
		assert_eq!(inner.next_target, Some(1));
		let shm = ShmRegionView::create(&shm_key("shrink"), 2, 64).expect("shm");
		let mut handle = WorkerHandle::shell(0, shm, 2, 64);
		handle.state = WorkerState::Alive;
		inner.workers.push(handle);
		// Open the throttle window so the next pump applies the target.
		inner.last_resize_at = Some(Instant::now() - MIN_RESIZE_INTERVAL - Duration::from_secs(1));
	}
	dispatcher.poll();
	assert_eq!(dispatcher.worker_count(), 1);
	{
		let mut inner = lock(&dispatcher.inner);
		assert_eq!(
			inner.workers.len(),
			1,
			"the retiring worker drained and was reaped"
		);
		assert_eq!(inner.next_target, None);
		// A Dead worker whose respawn fails stays Dead.
		inner.workers[0].state = WorkerState::Dead;
		inner.workers[0].retiring = false;
		inner.workers[0].restarts = 0;
	}
	dispatcher.poll();
	{
		let inner = lock(&dispatcher.inner);
		assert_eq!(inner.workers[0].state, WorkerState::Dead);
		assert_eq!(inner.workers[0].restarts, 1);
	}
	// The retiring-crash path removes the worker outright.
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("retire-crash"), 2, 64).expect("shm");
		let mut handle = WorkerHandle::shell(0, shm, 2, 64);
		handle.state = WorkerState::Dead;
		handle.retiring = true;
		inner.workers.push(handle);
		let index = inner.workers.len() - 1;
		let mut fired = Vec::new();
		dispatcher.restart_worker(&mut inner, index, &mut fired);
		assert!(fired.is_empty());
		assert_eq!(inner.workers.len(), index);
	}
}

#[cfg(unix)]
#[test]
fn retiring_worker_hung_past_deadline_is_killed() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 1)).expect("dispatcher");
	let mut child = std::process::Command::new("/bin/cat")
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.spawn()
		.expect("/bin/cat");
	let stdin = child.stdin.take();
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("hung-retire"), 1, 64).expect("shm");
		let mut handle = WorkerHandle::shell(0, shm, 1, 64);
		handle.state = WorkerState::Starting;
		handle.retiring = true;
		handle.stdin = stdin;
		handle.child = Some(child);
		// Pretend the shutdown signal was sent 31 s ago.
		handle.retire_sent_at = Some(Instant::now() - Duration::from_secs(31));
		inner.workers.push(handle);
	}
	// `poll` locks the dispatcher internally: never call it under the
	// guard (that deadlocks).
	dispatcher.poll();
	{
		let inner = lock(&dispatcher.inner);
		assert!(inner.workers.is_empty(), "hung retiring worker killed");
	}
}

#[cfg(unix)]
#[test]
fn start_handshake_timeout_is_bounded() {
	use std::os::unix::fs::PermissionsExt;
	let _lock = pool_test_lock();
	// A silent stand-in worker: it ignores the `--backend` argument and
	// stays alive without ever speaking the protocol.
	let script =
		std::env::temp_dir().join(format!("quercus-procpool-silent-{}.sh", std::process::id()));
	std::fs::write(&script, "#!/bin/sh\nsleep 5\n").expect("script");
	let mut perms = std::fs::metadata(&script).expect("meta").permissions();
	perms.set_mode(0o755);
	std::fs::set_permissions(&script, perms).expect("chmod");

	let mut config = test_config(1, 1);
	config.worker_bin = Some(script.clone());
	config.handshake_timeout_ms = 60;
	let dispatcher = ProcessDispatcher::new(config).expect("dispatcher");
	let err = dispatcher.start().expect_err("no handshake arrives");
	assert!(
		format!("{err}").contains("handshake"),
		"timeout error surfaced: {err}"
	);
	// Kill the silent worker (it would otherwise linger).
	{
		let mut inner = lock(&dispatcher.inner);
		if let Some(handle) = inner.workers.get_mut(0) {
			if let Some(mut c) = handle.child.take() {
				let _ = c.kill();
				let _ = c.wait();
			}
			handle.stdin = None;
		}
	}
	let _ = std::fs::remove_file(&script);
}

#[cfg(unix)]
#[test]
fn shutdown_drains_events_and_fires_open_tickets() {
	use std::os::unix::fs::PermissionsExt;
	let _lock = pool_test_lock();
	// A short-lived stand-in worker: alive for the first drain round,
	// gone before the kill deadline.
	let script = std::env::temp_dir().join(format!(
		"quercus-procpool-shutdown-{}.sh",
		std::process::id()
	));
	std::fs::write(&script, "#!/bin/sh\nsleep 0.1\n").expect("script");
	let mut perms = std::fs::metadata(&script).expect("meta").permissions();
	perms.set_mode(0o755);
	std::fs::set_permissions(&script, perms).expect("chmod");

	let mut config = test_config(1, 1);
	config.worker_bin = Some(script.clone());
	let dispatcher = ProcessDispatcher::new(config).expect("dispatcher");
	let mut child = std::process::Command::new(&script)
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.spawn()
		.expect("stand-in worker");
	let stdin = child.stdin.take();
	let shm = ShmRegionView::create(&shm_key("shutdown-drain"), 2, 64).expect("shm");
	let slot = publish_slot(&shm, 77);
	let results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	{
		let mut inner = lock(&dispatcher.inner);
		let mut handle = WorkerHandle::shell(0, shm.clone(), 2, 64);
		handle.state = WorkerState::Alive;
		handle.stdin = stdin;
		handle.child = Some(child);
		handle.outstanding.insert(21, slot);
		let sink = results.clone();
		inner.tickets.insert(
			21,
			PendingTicket {
				key: FrameKey {
					sequence: 1,
					frame: 0,
					version: 0,
				},
				params: Arc::new(video_params(0)),
				audio: None,
				done: Some(Box::new(move |result| {
					sink.lock().unwrap_or_else(|e| e.into_inner()).push(result)
				})),
			},
		);
		// A ticket with no frame in flight must fail with Error::State.
		let sink = results.clone();
		inner.tickets.insert(
			22,
			PendingTicket {
				key: FrameKey {
					sequence: 2,
					frame: 0,
					version: 0,
				},
				params: Arc::new(video_params(1)),
				audio: None,
				done: Some(Box::new(move |result| {
					sink.lock().unwrap_or_else(|e| e.into_inner()).push(result)
				})),
			},
		);
		inner.workers.push(handle);
		// Inject the frame_ready the fake worker would have sent, so the
		// shutdown drain delivers a completion.
		let line = format!(r#"{{"type":"frame_ready","ticket":21,"slot":{slot}}}"#);
		assert!(inner
			.events_tx
			.send(WorkerEvent::Line {
				worker: 0,
				generation: 0,
				line,
			})
			.is_ok());
	}
	dispatcher.shutdown();
	{
		let got = results.lock().unwrap();
		assert_eq!(got.len(), 2, "one rendered frame + one cancellation");
		assert!(got
			.iter()
			.any(|r| matches!(r, Ok(TicketPayload::ShmFrame(f)) if f.meta.id == 77)));
		assert!(got.iter().any(|r| r.is_err()));
	}
	let _ = std::fs::remove_file(&script);
}

#[test]
fn build_audio_ticket_spec_carries_montage() {
	let params = AudioTicketParams {
		viewer: 3,
		range: quercus_core::TimeRange::new(
			quercus_core::Rational::new(1, 2),
			quercus_core::Rational::new(3, 2),
		),
		sample_rate: 48000,
		channel_layout: 0x3,
		montage: vec![crate::ticket::MontageClip {
			filename: "clip.mp4".into(),
			stream_index: 1,
			in_time: quercus_core::Rational::new(1, 4),
			out_time: quercus_core::Rational::new(3, 4),
			media_in: quercus_core::Rational::new(0, 1),
			gain: 0.5,
			pan: 0.0,
			fade_in: quercus_core::Rational::new(0, 1),
			fade_out: quercus_core::Rational::new(0, 1),
			effects: Vec::new(),
			audio_effects: Vec::new(),
		}],
		bus_effects: vec![crate::ticket::MontageAudioEffect {
			type_id: "org.olivevideoeditor.Olive.limiter".into(),
			enabled: true,
			params: vec![(
				"ceiling_db".into(),
				quercus_node::value::NodeValue::Float(-1.0),
			)],
		}],
	};
	let spec = build_audio_ticket_spec(5, 2, &params);
	assert_eq!(spec.ticket, 5);
	assert_eq!(spec.slot, 2);
	assert_eq!(spec.time_num, 1);
	assert_eq!(spec.time_den, 2);
	assert_eq!(spec.duration_num, 1);
	assert_eq!(spec.duration_den, 1);
	assert_eq!(spec.sample_rate, 48000);
	assert_eq!(spec.channels, 2);
	assert_eq!(spec.montage.len(), 1);
	assert_eq!(spec.montage[0].filename, "clip.mp4");
	assert_eq!(spec.montage[0].stream_index, 1);
	assert_eq!(spec.montage[0].in_num, 1);
	assert_eq!(spec.montage[0].out_den, 4);
	assert_eq!(spec.montage[0].media_in_num, 0);
	assert_eq!(spec.montage[0].gain, 0.5);
	assert_eq!(spec.bus_effects.len(), 1);
	assert_eq!(
		spec.bus_effects[0].type_id,
		"org.olivevideoeditor.Olive.limiter"
	);
	assert_eq!(spec.bus_effects[0].params[0].input, "ceiling_db");
}

#[test]
fn real_worker_video_audio_round_trip_and_release_paths() {
	let _lock = pool_test_lock();
	let Some(bin) = find_real_worker() else {
		eprintln!("quercus-worker binary not found; run `cargo build -p quercus-worker`; skipping");
		return;
	};
	let config = DispatcherConfig {
		worker_bin: Some(bin),
		workers: 1,
		slots_per_worker: 4,
		width: 16,
		height: 16,
		slot_format: SLOT_FORMAT_BGRA8,
		batch_size: 2,
		graph_snapshot: None,
		handshake_timeout_ms: 60_000,
	};
	let dispatcher = ProcessDispatcher::new(config).expect("dispatcher");
	dispatcher.start().expect("worker starts");
	assert!(dispatcher.is_alive(0));
	assert!(dispatcher.shm_of(0).is_some());
	assert_eq!(dispatcher.slot_bytes(), 16 * 16 * 4);
	assert!(dispatcher.preview_window_capacity() >= 1);

	// Plugin-cancel broadcast to a live worker.
	request_plugin_cancel_all();
	// Graph snapshot push (the worker may reject the path; the send
	// succeeds) and clear.
	dispatcher.set_graph_snapshot(Some("/nonexistent/quercus-graph.xml".into()));
	dispatcher.poll();
	dispatcher.set_graph_snapshot(None);

	// A generated-frame video ticket renders into a BGRA8 slot.
	let results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	assert!(dispatcher.post(test_job(1, 0, None, &results)));
	pump_until(&dispatcher, &results, 1);
	let result = results.lock().unwrap().pop().unwrap();
	let Ok(TicketPayload::ShmFrame(frame)) = result else {
		panic!("ShmFrame expected");
	};
	assert_eq!(frame.meta.width, 16);
	assert_eq!(frame.meta.height, 16);
	assert_eq!(frame.meta.format, SLOT_FORMAT_BGRA8);
	// A stale ref (different segment) is ignored.
	let other = ShmRegionView::create(&shm_key("stale-release"), 1, 64).expect("shm");
	let stale = ShmFrameRef {
		worker: 0,
		slot: frame.slot,
		meta: frame.meta.clone(),
		shm: other,
	};
	dispatcher.release_frame(&stale);
	dispatcher.release_frame(&frame);
	dispatcher.release_frame(&frame); // double release: ignored

	// An audio ticket: the range exceeds the slot, so the dispatcher
	// grows the worker's segment before claiming.
	let audio = Arc::new(AudioTicketParams {
		viewer: 1,
		range: quercus_core::TimeRange::new(
			quercus_core::Rational::new(0, 1),
			quercus_core::Rational::new(1, 48),
		),
		sample_rate: 48000,
		channel_layout: 0x3,
		montage: Vec::new(),
		bus_effects: Vec::new(),
	});
	let audio_results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	assert!(dispatcher.post(test_job(2, 0, Some(audio), &audio_results)));
	pump_until(&dispatcher, &audio_results, 1);
	let result = audio_results.lock().unwrap().pop().unwrap();
	let Ok(TicketPayload::ShmAudio(audio)) = result else {
		panic!("ShmAudio expected");
	};
	assert_eq!(audio.sample_rate, 48000);
	assert_eq!(audio.channel_count, 2);
	JobDispatch::release_audio_frame(&*dispatcher, &audio);
	dispatcher.shutdown();
}

#[test]
fn real_worker_crash_restarts_and_requeues() {
	let _lock = pool_test_lock();
	let Some(bin) = find_real_worker() else {
		eprintln!("quercus-worker binary not found; run `cargo build -p quercus-worker`; skipping");
		return;
	};
	let marker =
		std::env::temp_dir().join(format!("quercus-procpool-crash-ut-{}", std::process::id()));
	let _ = std::fs::remove_file(&marker);
	let _crash = EnvRestore::set("QUERCUS_WORKER_CRASH_ON_TICKET", "1");
	let _marker = EnvRestore::set(
		"QUERCUS_WORKER_CRASH_MARKER",
		marker.to_string_lossy().as_ref(),
	);
	let config = DispatcherConfig {
		worker_bin: Some(bin),
		workers: 1,
		slots_per_worker: 4,
		width: 16,
		height: 16,
		slot_format: SLOT_FORMAT_BGRA8,
		batch_size: 2,
		graph_snapshot: None,
		handshake_timeout_ms: 60_000,
	};
	let dispatcher = ProcessDispatcher::new(config).expect("dispatcher");
	dispatcher.start().expect("worker starts");
	let results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	assert!(dispatcher.post(test_job(1, 0, None, &results)));
	assert!(dispatcher.post(test_job(1, 1, None, &results)));
	pump_until(&dispatcher, &results, 2);
	let restarts = dispatcher.restarts_of(0);
	assert!(
		restarts >= 1,
		"the crashed worker restarted (restarts={restarts})"
	);
	let mut seen = 0usize;
	for result in results.lock().unwrap().drain(..) {
		let payload = result.expect("frame rendered despite the crash");
		if let TicketPayload::ShmFrame(frame) = payload {
			seen += 1;
			dispatcher.release_frame(&frame);
		}
	}
	assert_eq!(seen, 2);
	assert!(marker.exists(), "the crash hook fired");
	let _ = std::fs::remove_file(&marker);
	dispatcher.shutdown();
}

#[test]
fn plugin_cancel_broadcast_marks_dead_and_snapshot_after_shutdown() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 1)).expect("dispatcher");
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("plugin-cancel"), 1, 64).expect("shm");
		inner.workers.push(WorkerHandle::shell(0, shm, 1, 64)); // Starting, no stdin
	}
	// Point the process-wide slot at this dispatcher so the broadcast
	// reaches it deterministically.
	*dispatcher_slot().lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(&dispatcher);
	request_plugin_cancel_all();
	{
		let inner = lock(&dispatcher.inner);
		assert_eq!(inner.workers[0].state, WorkerState::Dead);
	}
	// After shutdown the graph-snapshot setter is a no-op.
	dispatcher.shutdown();
	dispatcher.set_graph_snapshot(Some("/nonexistent/graph.xml".into()));
	{
		let inner = lock(&dispatcher.inner);
		assert!(inner.config.graph_snapshot.is_none());
	}
}

#[test]
fn stale_generation_events_are_dropped() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 1)).expect("dispatcher");
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("stale-events"), 1, 64).expect("shm");
		let mut handle = WorkerHandle::shell(5, shm, 1, 64);
		handle.state = WorkerState::PermanentlyDead;
		let tx = inner.events_tx.clone();
		inner.workers.push(handle);
		// Events from an older spawn generation are dropped.
		assert!(tx
			.send(WorkerEvent::Line {
				worker: 0,
				generation: 4,
				line: r#"{"type":"batch_accepted","batch_id":1,"tickets":[]}"#.into(),
			})
			.is_ok());
		assert!(tx
			.send(WorkerEvent::Eof {
				worker: 0,
				generation: 4,
			})
			.is_ok());
		// The current generation's EOF is applied (the state is kept
		// PermanentlyDead rather than downgraded to Dead).
		assert!(tx
			.send(WorkerEvent::Eof {
				worker: 0,
				generation: 5,
			})
			.is_ok());
	}
	dispatcher.poll();
	{
		let inner = lock(&dispatcher.inner);
		assert_eq!(inner.workers[0].accepted_batches, 0, "stale line dropped");
		assert_eq!(inner.workers[0].state, WorkerState::PermanentlyDead);
	}
}

// ---- Branch-coverage fill-ins: control-plane send failures and
// ---- environment-dependent query paths -------------------------------

/// `set_graph_snapshot(Some(..))` on a worker whose stdin is gone marks
/// it Dead (the send failure recycles it); clearing still only updates
/// the config.
#[test]
fn set_graph_snapshot_marks_dead_on_send_failure() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 1)).expect("dispatcher");
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("graph-send"), 1, 64).expect("shm");
		let mut handle = WorkerHandle::shell(0, shm, 1, 64);
		handle.state = WorkerState::Alive;
		inner.workers.push(handle);
	}
	dispatcher.set_graph_snapshot(Some("/nonexistent/quercus-graph.xml".into()));
	{
		let inner = lock(&dispatcher.inner);
		assert!(inner.workers[0].graph_sent);
		assert_eq!(inner.workers[0].state, WorkerState::Dead);
		assert_eq!(
			inner.config.graph_snapshot.as_deref(),
			Some("/nonexistent/quercus-graph.xml")
		);
	}
	dispatcher.set_graph_snapshot(None);
	assert!(lock(&dispatcher.inner).config.graph_snapshot.is_none());
}

/// Restart budget exhausted: the reclaimed frames of the dead worker
/// fail permanently and the worker stays `PermanentlyDead`.
#[test]
fn restart_budget_exhausted_fails_reclaimed_frames() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 2)).expect("dispatcher");
	let results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	let key = FrameKey {
		sequence: 42,
		frame: 0,
		version: 0,
	};
	let mut fired = Vec::new();
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("restart-budget"), 2, 64).expect("shm");
		let mut handle = WorkerHandle::shell(0, shm, 2, 64);
		handle.state = WorkerState::Dead;
		// The next restart exceeds MAX_RESTARTS.
		handle.restarts = MAX_RESTARTS + 1;
		inner.workers.push(handle);
		inner.scheduler.submit(FrameRequest {
			key,
			priority: crate::scheduler::FramePriority::Seek,
			distance: 0,
			payload: 1,
			slot_bytes: 64,
		});
		let claimed = inner.scheduler.claim_batch(0, 2, 64).expect("claimed");
		assert_eq!(claimed.frames.len(), 1, "the frame is in flight");
		let sink = results.clone();
		inner.tickets.insert(
			1,
			PendingTicket {
				key,
				params: Arc::new(video_params(0)),
				audio: None,
				done: Some(Box::new(move |result| {
					sink.lock().unwrap_or_else(|e| e.into_inner()).push(result)
				})),
			},
		);
		dispatcher.restart_worker(&mut inner, 0, &mut fired);
		assert_eq!(inner.workers[0].state, WorkerState::PermanentlyDead);
		assert!(inner.tickets.is_empty(), "the ticket was reaped");
	}
	assert_eq!(fired.len(), 1);
	for (done, result) in fired {
		done(result);
	}
	let got = results.lock().unwrap();
	assert_eq!(got.len(), 1);
	assert!(got[0].is_err(), "the dropped frame fails permanently");
}

/// An audio-only batch on a worker without stdin: the video message is
/// skipped and the audio send failure recycles the worker.
#[test]
fn dispatch_to_audio_only_missing_stdin_recycles_the_worker() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 1)).expect("dispatcher");
	let key = FrameKey {
		sequence: 12,
		frame: 0,
		version: 0,
	};
	{
		let mut inner = lock(&dispatcher.inner);
		let shm = ShmRegionView::create(&shm_key("audio-no-stdin"), 1, 256).expect("shm");
		let mut handle = WorkerHandle::shell(0, shm, 1, 256);
		handle.state = WorkerState::Alive;
		inner.workers.push(handle);
		inner.tickets.insert(
			10,
			PendingTicket {
				key,
				params: Arc::new(video_params(0)),
				audio: Some(Arc::new(AudioTicketParams {
					viewer: 1,
					range: quercus_core::TimeRange::new(
						quercus_core::Rational::new(0, 1),
						quercus_core::Rational::new(1, 480),
					),
					sample_rate: 48000,
					channel_layout: 0x3,
					montage: Vec::new(),
					bus_effects: Vec::new(),
				})),
				done: None,
			},
		);
		inner.scheduler.submit(FrameRequest {
			key,
			priority: crate::scheduler::FramePriority::Seek,
			distance: 0,
			payload: 10,
			slot_bytes: 64,
		});
		dispatcher.dispatch_to(&mut inner, 0);
		assert_eq!(inner.workers[0].state, WorkerState::Dead);
		assert_eq!(
			inner.workers[0].outstanding.len(),
			1,
			"the slot was assigned before the failed send"
		);
	}
}

#[test]
fn on_frame_ready_for_unknown_worker_is_ignored() {
	let _lock = pool_test_lock();
	let dispatcher = ProcessDispatcher::new(test_config(1, 1)).expect("dispatcher");
	let mut fired = Vec::new();
	{
		let mut inner = lock(&dispatcher.inner);
		// No worker at index 99: the completion path returns early.
		dispatcher.on_frame_ready(&mut inner, 99, 1, 0, &mut fired);
	}
	assert!(fired.is_empty());
}

/// The producer in `test_job` is a stub (the process backend renders
/// from the wire spec, never in-process); invoking it documents and
/// covers the never-taken inline path.
#[test]
fn test_job_produce_stub_errors() {
	let results: Arc<Mutex<Vec<TicketResult>>> = Arc::new(Mutex::new(Vec::new()));
	let job = test_job(1, 0, None, &results);
	let out = (job.produce)(quercus_core::Rational::new(0, 1), &job.params);
	assert!(
		out.is_err(),
		"the process backend never runs the in-process producer"
	);
}

#[test]
fn env_restore_restores_previous_value() {
	let _lock = pool_test_lock();
	let key = "QUERCUS_PROCPOOL_TEST_RESTORE";
	std::env::set_var(key, "old");
	{
		let _restore = EnvRestore::set(key, "new");
		assert_eq!(std::env::var(key).as_deref(), Ok("new"));
	}
	assert_eq!(
		std::env::var(key).as_deref(),
		Ok("old"),
		"a previously set value is restored, not removed"
	);
	std::env::remove_var(key);
}

#[test]
fn find_real_worker_honors_existing_env_override() {
	let _lock = pool_test_lock();
	// The override must name a file that exists on every platform
	// (`/bin/sh` never does on Windows): the test's own executable.
	let worker = std::env::current_exe().expect("test executable");
	let _env = EnvRestore::set("QUERCUS_WORKER_BIN", worker.to_string_lossy().as_ref());
	assert_eq!(
		find_real_worker().as_deref(),
		Some(worker.as_path()),
		"an existing QUERCUS_WORKER_BIN wins over the sibling probe"
	);
}

/// The vram probes are environment-dependent (NVIDIA CLI, DRM sysfs);
/// the query must never panic and the capacity either reports a sane
/// bound or falls back to the RAM policy.
#[test]
fn gpu_vram_query_smoke() {
	let _ = nvidia_vram_bytes();
	let _ = gpu_vram_bytes();
	let cap = gpu_worker_capacity((1920, 1080), 30);
	assert!(cap.is_none() || cap.unwrap() >= 1);
}

/// With the NVIDIA CLI unavailable the probe falls through to the
/// Linux DRM sysfs walk (or the `None` fallback on hosts without DRM
/// attrs). PATH is narrowed only for the duration of this locked test.
#[cfg(target_os = "linux")]
#[test]
fn gpu_vram_bytes_falls_back_to_drm_without_nvidia_cli() {
	let _lock = pool_test_lock();
	let _path = EnvRestore::set("PATH", "/nonexistent-quercus-vram-probe");
	assert!(
		nvidia_vram_bytes().is_none(),
		"nvidia-smi cannot be spawned without PATH"
	);
	// Exercises the sysfs arm (or the None fallback) of gpu_vram_bytes.
	let _ = gpu_vram_bytes();
}

/// Every `nvidia-smi` response shape: non-zero exit, non-UTF-8 output,
/// a missing/malformed CSV pair, a negative free value and a zero
/// total are all rejected; a valid pair converts MiB to bytes.
#[cfg(unix)]
#[test]
fn nvidia_vram_bytes_rejects_every_bad_query() {
	use std::os::unix::fs::PermissionsExt;
	let _lock = pool_test_lock();
	let dir = std::env::temp_dir().join(format!("quercus-vram-fake-smi-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).expect("fixture dir");
	let script = dir.join("nvidia-smi");
	let write = |body: &str| {
		std::fs::write(&script, body).expect("script");
		let mut perms = std::fs::metadata(&script).expect("meta").permissions();
		perms.set_mode(0o755);
		std::fs::set_permissions(&script, perms).expect("chmod");
	};
	let _path = EnvRestore::set("PATH", dir.to_str().expect("utf8 path"));

	// Non-zero exit -> None.
	write("#!/bin/sh\nexit 1\n");
	assert!(nvidia_vram_bytes().is_none(), "failed query");
	// Non-UTF-8 stdout -> None.
	write("#!/bin/sh\nprintf '\\377\\376'\n");
	assert!(nvidia_vram_bytes().is_none(), "non-UTF-8 query");
	// No comma in the first line -> None.
	write("#!/bin/sh\nprintf '16155\\n'\n");
	assert!(nvidia_vram_bytes().is_none(), "missing separator");
	// Unparsable numbers -> None.
	write("#!/bin/sh\nprintf 'abc, 24576\\n'\n");
	assert!(nvidia_vram_bytes().is_none(), "unparsable free");
	// Negative free (driver 'unknown') -> None.
	write("#!/bin/sh\nprintf ' -1, 24576\\n'\n");
	assert!(nvidia_vram_bytes().is_none(), "negative free");
	// Zero total -> None.
	write("#!/bin/sh\nprintf '100, 0\\n'\n");
	assert!(nvidia_vram_bytes().is_none(), "zero total");
	// A valid pair converts MiB -> bytes.
	write("#!/bin/sh\nprintf '100, 200\\n'\n");
	assert_eq!(nvidia_vram_bytes(), Some((100 << 20, 200 << 20)));

	drop(_path);
	let _ = std::fs::remove_dir_all(&dir);
}

/// The DRM walk skips a card whose `total` is unparsable and treats an
/// unparsable `used` as zero, then picks the next usable card.
#[cfg(target_os = "linux")]
#[test]
fn linux_drm_vram_walk_skips_unparsable_total() {
	let dir = std::env::temp_dir().join(format!(
		"quercus_vram_fixture_bad_{}_{}",
		std::process::id(),
		std::thread::current().name().unwrap_or("t")
	));
	let _ = std::fs::remove_dir_all(&dir);
	// card0: an unparsable total -> skipped by the walk.
	let card0 = dir.join("card0").join("device");
	std::fs::create_dir_all(&card0).unwrap();
	std::fs::write(card0.join("mem_info_vram_total"), "not-a-number").unwrap();
	// card1: a valid total, but an unparsable `used` counts as zero.
	let card1 = dir.join("card1").join("device");
	std::fs::create_dir_all(&card1).unwrap();
	std::fs::write(card1.join("mem_info_vram_total"), (2u64 << 30).to_string()).unwrap();
	std::fs::write(card1.join("mem_info_vram_used"), "garbage").unwrap();

	let (free, total) = linux_drm_vram_bytes_from(&dir).expect("card1 wins");
	assert_eq!(total, 2 << 30);
	assert_eq!(free, 2 << 30, "an unparsable `used` counts as zero");
	let _ = std::fs::remove_dir_all(&dir);
}

/// The spawn reader turns a non-UTF-8 stdout line into an EOF event
/// (`BufRead::read_line` errors on invalid UTF-8) instead of spinning.
#[cfg(unix)]
#[test]
fn spawn_worker_reader_reports_eof_on_invalid_utf8() {
	use std::os::unix::fs::PermissionsExt;
	let _lock = pool_test_lock();
	let script = std::env::temp_dir().join(format!(
		"quercus-procpool-badutf8-{}.sh",
		std::process::id()
	));
	std::fs::write(&script, "#!/bin/sh\nprintf '\\377\\n'\nsleep 5\n").expect("script");
	let mut perms = std::fs::metadata(&script).expect("meta").permissions();
	perms.set_mode(0o755);
	std::fs::set_permissions(&script, perms).expect("chmod");

	let dispatcher = ProcessDispatcher::new(test_config(1, 1)).expect("dispatcher");
	let mut inner = lock(&dispatcher.inner);
	inner.bin = script.clone();
	dispatcher.spawn_worker(&mut inner, 0).expect("spawn");
	// Bounded wait for the reader's EOF event.
	let deadline = Instant::now() + Duration::from_secs(10);
	let mut eof = false;
	while !eof && Instant::now() < deadline {
		while let Ok(ev) = inner.events_rx.try_recv() {
			if matches!(ev, WorkerEvent::Eof { worker: 0, .. }) {
				eof = true;
			}
		}
		if !eof {
			std::thread::sleep(Duration::from_millis(2));
		}
	}
	assert!(eof, "invalid UTF-8 must surface as an EOF event");
	if let Some(mut child) = inner.workers[0].child.take() {
		let _ = child.kill();
		let _ = child.wait();
	}
	drop(inner);
	let _ = std::fs::remove_file(&script);
}
