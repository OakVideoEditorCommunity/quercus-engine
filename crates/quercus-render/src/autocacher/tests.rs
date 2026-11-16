use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::worker::{InlineDispatcher, JobDispatch};
use quercus_core::frame::VideoParamsPod;
use quercus_core::texture::{Frame, Texture};

fn frame_producer() -> crate::ticket::Producer {
	Arc::new(|_, _| {
		let mut f = Frame::new();
		let mut p = VideoParamsPod::default();
		p.width = 4;
		p.height = 4;
		f.set_video_params(p);
		f.allocate();
		Ok(crate::ticket::TicketPayload::Video(Texture::wrap_frame(f)))
	})
}

/// A cacher on a queued inline dispatcher: jobs run only when the test
/// drains it with `InlineDispatcher::run` (deterministic ordering, no
/// worker threads).
fn new_cacher() -> (PreviewAutoCacher, Arc<InlineDispatcher>) {
	let d = InlineDispatcher::queued();
	let arena = Arc::new(TicketArena::new(d.clone(), frame_producer()));
	(PreviewAutoCacher::new(arena), d)
}

/// A dispatcher that records the posted schedules instead of running
/// them (the priority test only needs the posted `JobSchedule`).
#[derive(Default)]
struct RecordingDispatcher {
	schedules: Mutex<Vec<crate::worker::JobSchedule>>,
}

impl JobDispatch for RecordingDispatcher {
	fn post(&self, job: crate::worker::Job) -> bool {
		lock(&self.schedules).push(job.schedule);
		true
	}

	fn shutdown(&self) {}
}

#[test]
fn range_jobs_are_background_and_single_frames_stay_seek() {
	let d = Arc::new(RecordingDispatcher::default());
	let arena = Arc::new(TicketArena::new(d.clone(), frame_producer()));
	let mut c = PreviewAutoCacher::new(arena);
	c.attach(7).unwrap();
	c.on_cache_request(7, TimeRange::new(Rational::new(0, 1), Rational::new(10, 1)));
	c.single_frame(Rational::new(0, 1));
	let schedules = lock(&d.schedules);
	assert_eq!(schedules.len(), 2, "one range job + one single frame");
	assert_eq!(
		schedules[0].priority,
		crate::scheduler::FramePriority::Background,
		"autocache must yield to playback (§3.3)"
	);
	assert_eq!(
		schedules[1].priority,
		crate::scheduler::FramePriority::Seek,
		"the interactive single-frame preview stays Seek"
	);
}

#[test]
fn attach_detach_lifecycle() {
	let (mut c, d) = new_cacher();
	assert!(c.attach(0).is_err(), "zero identity rejected");
	c.attach(42).unwrap();
	assert_eq!(c.copied_project, 42);
	c.on_cache_request(
		42,
		TimeRange::new(Rational::new(0, 1), Rational::new(10, 1)),
	);
	assert_eq!(c.live_jobs().len(), 1);
	c.detach();
	assert_eq!(c.copied_project, 0);
	assert!(c.live_jobs().is_empty());
	d.shutdown();
}

#[test]
fn single_frame_cancels_previous() {
	// The race this asserts ("the previous frame is cancelled") is
	// deterministic on the queued inline dispatcher: the first job stays
	// queued until `run`, so the superseding submit's cancel lands first.
	let (mut c, d) = new_cacher_slow();
	c.attach(7).unwrap();
	let first = c.single_frame(Rational::new(0, 1));
	let second = c.single_frame(Rational::new(1, 1));
	assert_ne!(first, second);
	// The first ticket is cancelled: its completion fired with State.
	d.run();
	c.arena.wait(first).unwrap();
	let r = c.arena.result(first).unwrap();
	assert!(r.is_err());
	d.shutdown();
}

/// A cacher whose frames take ~100ms to produce (see
/// [`single_frame_cancels_previous`] — the sleep is irrelevant on the
/// queued dispatcher, kept for the original async-backend semantics).
fn new_cacher_slow() -> (PreviewAutoCacher, Arc<InlineDispatcher>) {
	let d = InlineDispatcher::queued();
	let producer: crate::ticket::Producer = Arc::new(|_, _| {
		std::thread::sleep(std::time::Duration::from_millis(100));
		let mut f = Frame::new();
		let mut p = VideoParamsPod::default();
		p.width = 4;
		p.height = 4;
		f.set_video_params(p);
		f.allocate();
		Ok(crate::ticket::TicketPayload::Video(Texture::wrap_frame(f)))
	});
	let arena = Arc::new(TicketArena::new(d.clone(), producer));
	(PreviewAutoCacher::new(arena), d)
}

#[test]
fn ignore_requests_suppresses_jobs() {
	let (mut c, d) = new_cacher();
	c.attach(7).unwrap();
	c.ignore_requests = true;
	c.on_cache_request(7, TimeRange::new(Rational::new(0, 1), Rational::new(5, 1)));
	assert!(c.live_jobs().is_empty());
	d.shutdown();
}

#[test]
fn renders_paused_queues_but_does_not_start() {
	let (mut c, d) = new_cacher();
	c.attach(7).unwrap();
	c.renders_paused = true;
	c.on_cache_request(7, TimeRange::new(Rational::new(0, 1), Rational::new(5, 1)));
	assert!(c.live_jobs().is_empty(), "paused: no jobs started");
	assert_eq!(c.pending_requests().len(), 1);
	d.shutdown();
}

#[test]
fn cancel_video_tasks_wait_blocks_until_idle() {
	let (mut c, d) = new_cacher();
	c.attach(7).unwrap();
	c.force_range(TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)));
	assert!(c.is_rendering_custom_range() || c.live_jobs().len() == 1);
	// Drain so the wait below observes every job finished.
	d.run();
	c.cancel_video_tasks(true);
	assert!(c.live_jobs().iter().all(|id| c.arena.is_finished(*id)));
	assert!(!c.is_rendering_custom_range());
	d.shutdown();
}

struct Probe {
	progress: AtomicU32,
	stop: AtomicU32,
}

#[test]
fn events_deliver_progress() {
	let (mut c, d) = new_cacher();
	let probe = Arc::new(Probe {
		progress: AtomicU32::new(0),
		stop: AtomicU32::new(0),
	});
	let probe2 = probe.clone();
	c.set_events(Box::new(ProbeEvents { probe: probe2 }));
	c.report_progress(0.5);
	c.request_stop_proxy_tasks();
	assert_eq!(probe.progress.load(Ordering::Relaxed), 1);
	assert_eq!(probe.stop.load(Ordering::Relaxed), 1);
	d.shutdown();
}

struct ProbeEvents {
	probe: Arc<Probe>,
}
impl AutoCacheEvents for ProbeEvents {
	fn progress(&mut self, value: f64) {
		// The reported fraction must arrive intact.
		assert_eq!(value, 0.5);
		self.probe.progress.fetch_add(1, Ordering::Relaxed);
	}
	fn stop_proxy_tasks(&mut self) {
		self.probe.stop.fetch_add(1, Ordering::Relaxed);
	}
}

#[test]
fn clear_finished_single_frames_removes_done() {
	let (mut c, d) = new_cacher();
	c.attach(7).unwrap();
	let id = c.single_frame(Rational::new(0, 1));
	d.run();
	c.arena.wait(id).unwrap();
	assert_eq!(c.live_jobs().len(), 1, "finished job still tracked");
	c.clear_finished_single_frames();
	assert!(c.live_jobs().is_empty());
	d.shutdown();
}

#[test]
fn noop_events_sink_never_panics() {
	let mut c = {
		let d = InlineDispatcher::sync();
		let arena = Arc::new(TicketArena::new(d, frame_producer()));
		PreviewAutoCacher::new(arena)
	};
	c.set_events(Box::new(NoopEvents));
	c.report_progress(1.0);
	c.request_stop_proxy_tasks();
}
