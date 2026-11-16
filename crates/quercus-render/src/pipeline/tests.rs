use super::*;
use crate::worker::JobSchedule;
use quercus_core::texture::Frame;

/// A unique clip per test (the process id separates test binaries, the
/// tag separates tests inside one binary — the decode caches are
/// process-wide).
fn test_clip(tag: &str) -> std::path::PathBuf {
	let path = std::env::temp_dir().join(format!(
		"quercusrender_pipeline_{tag}_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip(&path, 64, 64, 10, 10).expect("test clip generation");
	path
}

/// Pin the working space to the legacy sRGB pass-through and hold the
/// crate-wide working-space test lock for the entire decoded-pattern
/// section: these tests assert the decoded pattern, not the color
/// transform (the ACEScg default would remap the values), while the
/// eval tests temporarily switch the same process-global settings. The
/// caller must keep the returned guard alive.
fn pin_legacy_working_space() -> std::sync::MutexGuard<'static, ()> {
	let guard = crate::eval::working_space_test_lock()
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	quercus_core::color::set_pipeline_color_settings(
		quercus_core::colormath::WorkingColorSpace::SrgbLegacy,
		quercus_core::colormath::OutputColorSpec::default(),
	);
	guard
}

fn request(filename: &std::path::Path, time: Rational) -> DecodeRequest {
	DecodeRequest {
		filename: filename.to_string_lossy().to_string(),
		stream_index: 0,
		time,
		size: (64, 64),
		format: PixelFormat::F32,
		allow_import: false,
	}
}

fn always() -> PrefetchGate {
	Arc::new(|| true)
}

fn frame_of(texture: &Texture) -> &Frame {
	let Texture::Cpu(frame) = texture else {
		panic!("decode produced a non-CPU texture");
	};
	frame
}

/// The decoded test pattern: a red|blue split that steps with the frame
/// index — `quercus_codec::testmedia` shifts it by `index * width / (2 * fps)`
/// columns (9 columns for frame 3 of this 64 px / 10 fps clip). The
/// sampled columns track that shift; MPEG-2 is lossy, so the assertions
/// use dominance with generous margins.
fn assert_known_pattern(frame: &Frame, index: i32, tag: &str) {
	assert_eq!((frame.width, frame.height), (64, 64), "{tag}");
	assert_eq!(frame.format, PixelFormat::F32, "{tag}");
	let stride = frame.linesize_bytes();
	let shift = (index * frame.width / 20).rem_euclid(frame.width);
	let read = |x: usize, y: usize| -> [f32; 4] {
		let off = y * stride + x * 16;
		let mut out = [0f32; 4];
		for i in 0..4 {
			out[i] =
				f32::from_le_bytes(frame.data[off + i * 4..off + i * 4 + 4].try_into().unwrap());
		}
		out
	};
	// Sample the middle of each half: the generator's split has the red
	// half where `(x + shift) % 64` is below 32.
	let [r, g, b, a] = read((16 - shift).rem_euclid(64) as usize, 32);
	assert!(r > 0.5 && g < 0.4 && b < 0.4, "{tag}: red half {r},{g},{b}");
	assert!(a > 0.9, "{tag}: opaque {a}");
	let [r, g, b, a] = read((48 - shift).rem_euclid(64) as usize, 32);
	assert!(
		b > 0.5 && r < 0.4 && g < 0.4,
		"{tag}: blue half {r},{g},{b}"
	);
	assert!(a > 0.9, "{tag}: opaque {a}");
}

/// The service decodes real media through the real codec path.
#[test]
fn request_decodes_real_media() {
	let _guard = pin_legacy_working_space();
	let path = test_clip("real");
	let service = DecodeService::new(DECODE_LRU_CAP, always());

	let texture = service
		.request(request(&path, Rational::new(0, 1)))
		.expect("service available")
		.expect("frame 0 decodes");
	assert_known_pattern(frame_of(&texture), 0, "frame 0");

	let stats = service.stats();
	assert_eq!(stats.requests, 1);
	assert_eq!(stats.decodes, 1, "one real decode");
	assert_eq!(stats.lru_hits, 0);
	assert_eq!(stats.errors, 0);
	assert_eq!(service.lru_len(), 1);

	service.shutdown();
	let _ = std::fs::remove_file(&path);
}

/// Prefetch really decodes and really fills the LRU: the request that
/// follows is served from the cache with no decode at all.
#[test]
fn prefetch_then_request_hits_the_lru() {
	let _guard = pin_legacy_working_space();
	let path = test_clip("prefetch");
	let service = DecodeService::new(DECODE_LRU_CAP, always());

	let req = request(&path, Rational::new(3, 10));
	assert!(service.prefetch(req.clone()), "prefetch accepted");
	assert!(service.wait_idle(), "decode thread processed the prefetch");
	let after_prefetch = service.stats();
	assert_eq!(after_prefetch.prefetches, 1);
	assert_eq!(after_prefetch.decodes, 1, "the prefetch really decoded");
	assert_eq!(service.lru_len(), 1);

	// The rendezvous request for the same frame: served from the LRU,
	// so the decode counter does NOT move (this is the assertion that
	// distinguishes a real cache hit from a re-decode).
	let texture = service
		.request(req)
		.expect("service available")
		.expect("cached frame");
	assert_known_pattern(frame_of(&texture), 3, "prefetched frame");
	let after_request = service.stats();
	assert_eq!(after_request.lru_hits, 1);
	assert_eq!(after_request.decodes, after_prefetch.decodes);
	assert_eq!(after_request.requests, 1);

	service.shutdown();
	let _ = std::fs::remove_file(&path);
}

/// Decode failures travel back to the requester unchanged.
#[test]
fn request_propagates_decode_errors() {
	let service = DecodeService::new(DECODE_LRU_CAP, always());
	let req = DecodeRequest {
		filename: "/definitely/not/here.mp4".into(),
		stream_index: 0,
		time: Rational::new(0, 1),
		size: (64, 64),
		format: PixelFormat::F32,
		allow_import: true,
	};
	let err = service
		.request(req)
		.expect("service available")
		.expect_err("decoding a missing file must fail");
	let _ = err.code(); // an explainable error, not a panic
	let stats = service.stats();
	assert_eq!(stats.errors, 1);
	assert_eq!(stats.decodes, 1);
	assert_eq!(service.lru_len(), 0, "failures are not cached");
	service.shutdown();
}

/// The LRU is bounded and evicts least-recently-used first (an evicted
/// frame must be decoded again).
#[test]
fn lru_evicts_bounded() {
	let _guard = pin_legacy_working_space();
	let path = test_clip("evict");
	let service = DecodeService::new(2, always());
	let time = |n: i64| request(&path, Rational::new(n, 10));

	service.request(time(0)).unwrap().unwrap();
	service.request(time(1)).unwrap().unwrap();
	assert_eq!(service.lru_len(), 2);
	assert_eq!(service.stats().evictions, 0);

	// Frame 2 evicts frame 0 (the least recently used).
	service.request(time(2)).unwrap().unwrap();
	assert_eq!(service.lru_len(), 2, "capacity holds");
	assert_eq!(service.stats().evictions, 1);
	let decodes = service.stats().decodes;
	assert_eq!(decodes, 3);

	// Frame 0 was evicted: asking for it decodes again...
	service.request(time(0)).unwrap().unwrap();
	assert_eq!(service.stats().decodes, decodes + 1);
	assert_eq!(service.stats().lru_hits, 0);
	// ...while frame 2 (still cached) does not.
	service.request(time(2)).unwrap().unwrap();
	assert_eq!(service.stats().decodes, decodes + 1);
	assert_eq!(service.stats().lru_hits, 1);

	service.shutdown();
	let _ = std::fs::remove_file(&path);
}

/// The prefetch gate is consulted per message: a closed gate refuses
/// (and counts) without queueing anything.
#[test]
fn prefetch_gate_refuses_and_recovers() {
	let _guard = pin_legacy_working_space();
	let path = test_clip("gate");
	let open = Arc::new(AtomicBool::new(false));
	let gate_open = open.clone();
	let service = DecodeService::new(
		DECODE_LRU_CAP,
		Arc::new(move || gate_open.load(Ordering::Relaxed)),
	);

	let req = request(&path, Rational::new(1, 10));
	assert!(!service.prefetch(req.clone()), "closed gate refuses");
	let stats = service.stats();
	assert_eq!(stats.prefetch_refused, 1);
	assert_eq!(stats.prefetches, 0);
	assert_eq!(stats.decodes, 0, "nothing was decoded behind the gate");

	open.store(true, Ordering::Relaxed);
	assert!(service.prefetch(req.clone()), "open gate accepts");
	assert!(service.wait_idle());
	let stats = service.stats();
	assert_eq!(stats.prefetches, 1);
	assert_eq!(stats.decodes, 1);

	service.shutdown();
	let _ = std::fs::remove_file(&path);
}

/// `wait_idle` is a real barrier through the command queue: every
/// prefetch sent before it has been decoded when it returns.
#[test]
fn wait_idle_barrier_covers_queued_commands() {
	let _guard = pin_legacy_working_space();
	let path = test_clip("barrier");
	// The barrier test needs four live entries; the production
	// hand-off capacity is deliberately tiny (see DECODE_LRU_CAP), so
	// this test sizes its own service.
	let service = DecodeService::new(4, always());
	for n in 0..4 {
		assert!(service.prefetch(request(&path, Rational::new(n, 10))));
	}
	assert!(service.wait_idle());
	let stats = service.stats();
	assert_eq!(stats.prefetches, 4);
	assert_eq!(stats.decodes, 4);
	assert_eq!(service.lru_len(), 4);
	service.shutdown();
	let _ = std::fs::remove_file(&path);
}

/// After shutdown the service reports itself gone (`None`), so the
/// eval path falls back to decoding inline instead of failing frames.
#[test]
fn shutdown_makes_the_service_unavailable() {
	let _guard = pin_legacy_working_space();
	let path = test_clip("shutdown");
	let service = DecodeService::new(DECODE_LRU_CAP, always());
	service.shutdown();
	service.shutdown(); // idempotent
	assert!(service
		.request(request(&path, Rational::new(0, 1)))
		.is_none());
	assert!(!service.prefetch(request(&path, Rational::new(0, 1))));
	assert!(!service.wait_idle());
	let _ = std::fs::remove_file(&path);
}

// ---- M4: priority queues and playback read-ahead --------------------

/// A minimal job for queue-ordering tests (the producer/done are no-ops
/// and never run).
fn dummy_job(priority: FramePriority, frame: i64) -> Job {
	Job {
		node_identity: 1,
		time: Rational::new(frame, 1),
		params: Arc::new(crate::ticket::VideoTicketParams {
			viewer: 1,
			project: String::new(),
			time: Rational::new(frame, 1),
			force_size: Some((16, 16)),
			force_format: None,
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage: None,
			montage: Vec::new(),
			adjustments: Vec::new(),
		}),
		audio: None,
		produce: Arc::new(|_, _| Ok(crate::ticket::TicketPayload::Video(Texture::dummy()))),
		done: Box::new(|_| {}),
		schedule: JobSchedule {
			priority,
			frame: Some(frame),
			distance: frame,
			version: 0,
		},
		cancelled: None,
	}
}

/// M4: the decode queue serves a rendezvous request before queued
/// prefetches, while a `Sync` barrier keeps FIFO order.
#[test]
fn decode_queue_prioritizes_requests_over_prefetches() {
	let shared = Arc::new(DecodeShared {
		queue: Mutex::new(DecodeQueue {
			pending: VecDeque::new(),
			shutting_down: false,
		}),
		work: Condvar::new(),
		room: Condvar::new(),
	});
	let (sync_tx, _sync_rx) = mpsc::sync_channel(0);
	let (request_tx, _request_rx) = mpsc::sync_channel(0);
	let request = |filename: &str, time: Rational| DecodeRequest {
		filename: filename.to_string(),
		stream_index: 0,
		time,
		size: (16, 16),
		format: PixelFormat::F32,
		allow_import: true,
	};
	{
		let mut queue = lock(&shared.queue);
		queue
			.pending
			.push_back(DecodeCommand::Sync { reply: sync_tx });
		queue.pending.push_back(DecodeCommand::Prefetch {
			request: request("a.mp4", Rational::new(0, 1)),
		});
		queue.pending.push_back(DecodeCommand::Prefetch {
			request: request("a.mp4", Rational::new(1, 10)),
		});
		queue.pending.push_back(DecodeCommand::Request {
			request: request("a.mp4", Rational::new(2, 10)),
			reply: request_tx,
		});
	}
	// The request preempts the two earlier prefetches...
	assert!(matches!(
		pop_command(&shared),
		Some(DecodeCommand::Request { .. })
	));
	// ...but the Sync barrier stays ahead of the prefetches that were
	// sent before it (FIFO).
	assert!(matches!(
		pop_command(&shared),
		Some(DecodeCommand::Sync { .. })
	));
	assert!(matches!(
		pop_command(&shared),
		Some(DecodeCommand::Prefetch { .. })
	));
	assert!(matches!(
		pop_command(&shared),
		Some(DecodeCommand::Prefetch { .. })
	));
}

/// M4: the render queue is priority-ordered (Seek, Playback,
/// Background) and FIFO inside a class.
#[test]
fn render_queue_orders_seek_playback_background() {
	let mut queue = VecDeque::new();
	enqueue_job(&mut queue, dummy_job(FramePriority::Background, 0));
	enqueue_job(&mut queue, dummy_job(FramePriority::Playback, 1));
	enqueue_job(&mut queue, dummy_job(FramePriority::Seek, 2));
	enqueue_job(&mut queue, dummy_job(FramePriority::Playback, 3));
	let classes: Vec<u8> = queue.iter().map(job_class).collect();
	assert_eq!(classes, vec![0, 1, 1, 2]);
	let frames: Vec<i64> = queue
		.iter()
		.map(|job| job.schedule.frame.unwrap())
		.collect();
	assert_eq!(frames, vec![2, 1, 3, 0], "FIFO within a class");
}

/// M4 read-ahead: the derived decode requests mirror the montage eval
/// (media time, target size, F32) and skip clips that do not cover the
/// requested time.
#[test]
fn playback_decode_requests_mirror_montage_times() {
	let clip = |filename: &str, in_t: Rational, out_t: Rational, media_in: Rational| {
		crate::ticket::MontageClip {
			filename: filename.to_string(),
			stream_index: 0,
			in_time: in_t,
			out_time: out_t,
			media_in,
			gain: 1.0,
			pan: 0.0,
			fade_in: Rational::new(0, 1),
			fade_out: Rational::new(0, 1),
			effects: Vec::new(),
			audio_effects: Vec::new(),
		}
	};
	let params = crate::ticket::VideoTicketParams {
		viewer: 1,
		project: String::new(),
		time: Rational::new(5, 10),
		force_size: Some((64, 32)),
		force_format: None,
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage: vec![
			clip(
				"covered.mp4",
				Rational::new(0, 1),
				Rational::new(1, 1),
				Rational::new(2, 1),
			),
			clip(
				"outside.mp4",
				Rational::new(1, 1),
				Rational::new(2, 1),
				Rational::new(0, 1),
			),
		],
		adjustments: Vec::new(),
	};
	let requests = playback_decode_requests(&params, Rational::new(5, 10));
	assert_eq!(requests.len(), 1, "only the covering clip is prefetched");
	assert_eq!(requests[0].filename, "covered.mp4");
	assert_eq!(
		requests[0].time,
		Rational::new(5, 2),
		"media_in + (time - in)"
	);
	assert_eq!(requests[0].size, (64, 32));
	assert_eq!(requests[0].format, PixelFormat::F32);
	assert_eq!(
		requests[0].allow_import,
		montage_prefers_gpu(&params, PixelFormat::F32)
	);

	// A single-footage ticket prefetches its stream at the ticket time.
	let mut footage = params;
	footage.montage.clear();
	footage.footage = Some(("solo.mp4".to_string(), 2));
	let requests = playback_decode_requests(&footage, Rational::new(5, 10));
	assert_eq!(requests.len(), 1);
	assert_eq!(requests[0].filename, "solo.mp4");
	assert_eq!(requests[0].stream_index, 2);
	assert_eq!(requests[0].time, Rational::new(1, 2));
}
