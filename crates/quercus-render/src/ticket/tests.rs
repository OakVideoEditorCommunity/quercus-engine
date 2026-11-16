use super::*;
use std::sync::mpsc;
use std::time::Duration;

use crate::worker::{InlineDispatcher, JobDispatch};
use quercus_core::frame::VideoParamsPod;
use quercus_core::texture::Frame;

fn small_frame() -> Frame {
	let mut f = Frame::new();
	let mut p = VideoParamsPod::default();
	p.width = 4;
	p.height = 4;
	f.set_video_params(p);
	f.allocate();
	f
}

fn ok_producer() -> Producer {
	Arc::new(|_, _| Ok(TicketPayload::Video(Texture::wrap_frame(small_frame()))))
}

/// A queued inline dispatcher for video (deterministic cancel-race and
/// shutdown semantics) plus a sync inline dispatcher for audio (the
/// production audio mode). `run` drains the video queue.
fn test_arena(producer: Producer) -> (TicketArena, Arc<InlineDispatcher>) {
	let video = InlineDispatcher::queued();
	let audio = InlineDispatcher::sync();
	let arena = TicketArena::new_with_audio(video.clone(), audio, producer);
	(arena, video)
}

#[test]
fn completion_fires_exactly_once_on_success() {
	let (arena, video) = test_arena(ok_producer());

	let (tx, rx) = mpsc::channel();
	let id = arena.submit_video(
		VideoTicketParams {
			viewer: 1,
			project: String::new(),
			time: Rational::new(0, 1),
			force_size: Some((4, 4)),
			force_format: None,
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage: None,
			montage: Vec::new(),
			adjustments: Vec::new(),
		},
		Box::new(move |r| {
			let _ = tx.send(r.is_ok());
		}),
	);

	video.run();
	arena.wait(id).unwrap();
	let ok = rx.recv_timeout(Duration::from_secs(5)).unwrap();
	assert!(ok, "completion fired with success");
	assert!(
		rx.recv_timeout(Duration::from_millis(50)).is_err(),
		"exactly once"
	);

	// is_finished 必须在 result() 之前查：result() 是终止性读取
	// （连同条目一起回收，arena 表不能无限增长）。
	assert!(arena.is_finished(id));
	let res = arena.result(id).unwrap().unwrap();
	assert_eq!(
		match res {
			TicketPayload::Video(t) => t.size(),
			_ => panic!("video ticket"),
		},
		(4, 4)
	);
	video.shutdown();
}

#[test]
fn completion_fires_exactly_once_on_cancel() {
	// Queued mode: the job is posted but not run yet, so a cancel
	// before `run` must deliver State exactly once (the old worker-pool
	// test used a blocking producer; the inline queue makes the same
	// cancel-before-completion race deterministic without threads).
	let (arena, video) = test_arena(ok_producer());

	let (tx, rx) = mpsc::channel();
	let id = arena.submit_video(
		VideoTicketParams {
			viewer: 1,
			project: String::new(),
			time: Rational::new(0, 1),
			force_size: None,
			force_format: None,
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage: None,
			montage: Vec::new(),
			adjustments: Vec::new(),
		},
		Box::new(move |r| {
			let _ = tx.send(r);
		}),
	);

	// Cancel before the job runs.
	arena.cancel(id);
	video.run();
	arena.wait(id).unwrap();

	let res = rx.recv_timeout(Duration::from_secs(5)).unwrap();
	assert_eq!(res.unwrap_err().code(), Error::State.code());
	assert!(
		rx.recv_timeout(Duration::from_millis(50)).is_err(),
		"exactly once"
	);
	video.shutdown();
}

#[test]
fn cancel_of_unknown_id_is_ignored() {
	let (arena, video) = test_arena(ok_producer());
	arena.cancel(TicketId(12345));
	assert!(!arena.is_finished(TicketId(12345)));
	video.shutdown();
}

#[test]
fn wait_unknown_id_errors() {
	let (arena, video) = test_arena(ok_producer());
	assert_eq!(
		arena.wait(TicketId(999)).unwrap_err().code(),
		Error::NotFound.code()
	);
	video.shutdown();
}

#[test]
fn audio_ticket_meta_and_kind() {
	// Audio runs on the sync inline backend (the production audio
	// mode), so the completion fires during the submit.
	let (arena, video) = test_arena(ok_producer());

	let (tx, rx) = mpsc::channel();
	let range = TimeRange::new(Rational::new(0, 1), Rational::new(10, 1));
	let id = arena.submit_audio(
		AudioTicketParams {
			viewer: 7,
			range,
			sample_rate: 48000,
			channel_layout: 0x3,
			montage: Vec::new(),
			bus_effects: Vec::new(),
		},
		Box::new(move |r: TicketResult| {
			let _ = tx.send(r.is_err());
		}),
	);
	assert_eq!(arena.kind(id), Some(ticket_kind::AUDIO));
	assert_eq!(arena.meta(id).unwrap().kind, Some(ticket_kind::AUDIO));
	assert_eq!(arena.range(id), Some(range));
	arena.wait(id).unwrap();
	// M12 P1: an empty-montage audio ticket succeeds with silence
	// (the completion must NOT report an error).
	assert!(!rx.recv_timeout(Duration::from_secs(5)).unwrap());
	// get_time equivalent: audio tickets report range.in.
	assert_eq!(arena.time(id), Some(range.in_()));
	video.shutdown();
}

#[test]
fn ticket_ids_are_monotonic() {
	let (arena, video) = test_arena(ok_producer());
	let a = arena.submit_video(
		VideoTicketParams {
			viewer: 1,
			project: String::new(),
			time: Rational::new(0, 1),
			force_size: None,
			force_format: None,
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage: None,
			montage: Vec::new(),
			adjustments: Vec::new(),
		},
		Box::new(|_| {}),
	);
	let b = arena.submit_video(
		VideoTicketParams {
			viewer: 1,
			project: String::new(),
			time: Rational::new(1, 1),
			force_size: None,
			force_format: None,
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage: None,
			montage: Vec::new(),
			adjustments: Vec::new(),
		},
		Box::new(|_| {}),
	);
	assert!(b.0 > a.0);
	assert_ne!(a, b);
	video.shutdown();
}

/// The montage audio view of a clip is read from the block core in one
/// place (preview/export both use it), with the persisted ranges
/// clamped and the effect chain copied in source-first order.
#[test]
fn montage_audio_reads_and_clamps_clip_fields() {
	let mut core = quercus_node::block::BlockCore::default();
	let defaults = MontageAudio::from_block_core(&core);
	assert_eq!(defaults.gain, 1.0);
	assert_eq!(defaults.pan, 0.0);
	assert_eq!(defaults.fade_in, Rational::new(0, 1));
	assert_eq!(defaults.fade_out, Rational::new(0, 1));
	assert!(defaults.effects.is_empty());

	core.audio_gain = 9.0;
	core.audio_pan = -3.0;
	core.audio_fade_in = Rational::new(-1, 1);
	core.audio_fade_out = Rational::new(1, 2);
	let mut volume =
		quercus_node::block::ClipAudioEffect::new(quercus_node::block::AUDIO_EFFECT_VOLUME_TYPE_ID);
	volume.set_param(
		quercus_node::block::AUDIO_VOLUME_INPUT,
		quercus_node::value::NodeValue::Float(0.5),
	);
	let pan =
		quercus_node::block::ClipAudioEffect::new(quercus_node::block::AUDIO_EFFECT_PAN_TYPE_ID);
	core.audio_effects = vec![volume, pan];
	let audio = MontageAudio::from_block_core(&core);
	assert_eq!(audio.gain, 4.0, "gain is clamped to the persisted range");
	assert_eq!(audio.pan, -1.0, "pan is clamped to -1..1");
	assert_eq!(audio.fade_in, Rational::new(0, 1), "negative fade -> none");
	assert_eq!(audio.fade_out, Rational::new(1, 2));
	assert_eq!(audio.effects.len(), 2);
	assert_eq!(
		audio.effects[0].type_id,
		quercus_node::block::AUDIO_EFFECT_VOLUME_TYPE_ID
	);
	assert_eq!(
		audio.effects[0].param(quercus_node::block::AUDIO_VOLUME_INPUT),
		Some(&quercus_node::value::NodeValue::Float(0.5))
	);
	assert_eq!(
		audio.effects[1].type_id,
		quercus_node::block::AUDIO_EFFECT_PAN_TYPE_ID
	);

	// Hand-edited NaN values fall back to unity/centre instead of
	// poisoning the whole mix.
	core.audio_gain = f64::NAN;
	core.audio_pan = f64::NAN;
	let audio = MontageAudio::from_block_core(&core);
	assert_eq!(audio.gain, 1.0);
	assert_eq!(audio.pan, 0.0);
}
