use super::*;

fn req(seq: u64, frame: i64, prio: FramePriority) -> FrameRequest<u64> {
	FrameRequest {
		key: FrameKey {
			sequence: seq,
			frame,
			version: 0,
		},
		priority: prio,
		distance: frame,
		payload: frame as u64,
		slot_bytes: 0,
	}
}

/// Claim until nothing is claimable by any worker; returns
/// (worker, frame) pairs in claim order.
fn claim_all(s: &mut PreviewScheduler<u64>) -> Vec<(usize, i64)> {
	let mut out = Vec::new();
	loop {
		let mut progress = false;
		for w in 0..s.workers() {
			while let Some(batch) = s.claim_batch(w, 1024, usize::MAX) {
				for f in &batch.frames {
					out.push((w, f.key.frame));
				}
				progress = true;
			}
		}
		if !progress {
			break;
		}
	}
	out
}

#[test]
fn no_stealing_every_frame_claimed_exactly_once() {
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(3, 4);
	for f in 0..40 {
		assert!(matches!(
			s.submit(req(1, f, FramePriority::Playback)),
			SubmitOutcome::Accepted
		));
	}
	let claims = claim_all(&mut s);
	assert_eq!(claims.len(), 40, "every frame claimed");
	let mut frames: Vec<i64> = claims.iter().map(|(_, f)| *f).collect();
	frames.sort_unstable();
	frames.dedup();
	assert_eq!(frames.len(), 40, "no frame claimed twice (no stealing)");
	assert_eq!(s.pending_count(), 0);
	assert_eq!(s.claimed_count(), 40, "all claimed, none completed yet");
}

#[test]
fn interleave_adjacent_frames_on_different_workers() {
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(4, 2);
	for f in 0..16 {
		s.submit(req(1, f, FramePriority::Playback));
	}
	let claims = claim_all(&mut s);
	for (worker, frame) in &claims {
		assert_eq!(
			frame.rem_euclid(4),
			*worker as i64,
			"frame {frame} must be claimed by worker {}",
			frame.rem_euclid(4)
		);
	}
	// Adjacent frames are on different workers.
	let worker_of: HashMap<i64, usize> = claims.iter().map(|(w, f)| (*f, *w)).collect();
	for f in 0..15 {
		assert_ne!(worker_of[&f], worker_of[&(f + 1)]);
	}
}

#[test]
fn crash_requeues_to_any_healthy_worker() {
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(2, 8);
	for f in 0..8 {
		s.submit(req(1, f, FramePriority::Playback));
	}
	// Both workers claim their shards first.
	let batch0 = s.claim_batch(0, 8, 1024).unwrap();
	assert_eq!(batch0.frames.len(), 4); // frames 0,2,4,6
	let batch1 = s.claim_batch(1, 8, 1024).unwrap();
	assert_eq!(batch1.frames.len(), 4); // frames 1,3,5,7

	// Worker 0 crashes: its frames come back...
	let reclaimed = s.worker_crashed(0);
	assert_eq!(reclaimed.len(), 4);
	assert_eq!(s.claimed_count(), 4, "worker 1 keeps its own batch");
	assert_eq!(s.pending_count(), 4);
	assert_eq!(s.crash_requeued(), 4);

	// ...and worker 1 (NOT their shard) can claim them all.
	let batch = s.claim_batch(1, 8, 1024).unwrap();
	assert_eq!(batch.frames.len(), 4);
	let mut frames: Vec<i64> = batch.frames.iter().map(|f| f.key.frame).collect();
	frames.sort_unstable();
	assert_eq!(frames, vec![0, 2, 4, 6]);
}

#[test]
fn priority_seek_beats_playback_beats_background() {
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 100);
	// All frames on worker 0's shard (W=1): background first, then a
	// playback window, then a seek frame submitted last.
	for f in 0..5 {
		s.submit(req(1, f, FramePriority::Background));
	}
	for f in 10..15 {
		let mut r = req(1, f, FramePriority::Playback);
		r.distance = (f - 12).abs();
		s.submit(r);
	}
	s.submit(req(1, 100, FramePriority::Seek));

	let batch = s.claim_batch(0, 100, 1024).unwrap();
	let order: Vec<(FramePriority, i64)> = batch
		.frames
		.iter()
		.map(|f| (f.priority, f.key.frame))
		.collect();
	// Seek first...
	assert_eq!(order[0], (FramePriority::Seek, 100));
	// ...then playback by playhead distance (12 nearest first)...
	assert_eq!(
		order[1..6].iter().map(|(_, f)| *f).collect::<Vec<_>>(),
		vec![12, 11, 13, 10, 14]
	);
	// ...then background ascending.
	assert_eq!(
		order[6..].iter().map(|(_, f)| *f).collect::<Vec<_>>(),
		vec![0, 1, 2, 3, 4]
	);
}

#[test]
fn flow_control_credit_limits_batch() {
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 100);
	for f in 0..10 {
		s.submit(req(1, f, FramePriority::Playback));
	}
	// Zero credit claims nothing.
	assert!(s.claim_batch(0, 0, 1024).is_none());
	// Credit 3 claims 2 playback frames: the last slot stays free as
	// the per-worker interactive reserve (window frames are released
	// by UI-thread consumption, so a full drain can starve a
	// UI-blocking seek).
	let batch = s.claim_batch(0, 3, 1024).unwrap();
	assert_eq!(batch.frames.len(), 2);
	assert_eq!(s.pending_count(), 8);
}

#[test]
fn playback_claims_keep_the_reserve_but_seeks_may_use_it() {
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 100);
	for f in 0..4 {
		s.submit(req(1, f, FramePriority::Playback));
	}
	// One free slot: playback claims nothing (reserve kept)...
	assert!(s.claim_batch(0, 1, 1024).is_none());
	assert_eq!(s.pending_count(), 4);
	// ...but a Seek (interactive frame / real-time audio) may use it:
	// seeks complete on the worker without UI-thread involvement, so
	// taking the last slot cannot deadlock the UI.
	s.submit(req(1, 100, FramePriority::Seek));
	let batch = s.claim_batch(0, 1, 1024).unwrap();
	assert_eq!(batch.frames.len(), 1);
	assert_eq!(batch.frames[0].priority, FramePriority::Seek);
}

#[test]
fn seek_is_claimable_by_any_worker() {
	// The no-stealing shard rule applies to Playback frames (adjacent
	// frames finish together); a single urgent seek pinned to a full
	// worker would starve even while other workers idle.
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(4, 100);
	s.submit(req(1, 3, FramePriority::Seek)); // shard 3
	let batch = s.claim_batch(0, 4, 1024).unwrap();
	assert_eq!(batch.frames.len(), 1);
	assert_eq!(batch.frames[0].key.frame, 3);
	// Playback frames stay pinned to their shard.
	s.submit(req(1, 7, FramePriority::Playback)); // shard 3
	assert!(s.claim_batch(0, 4, 1024).is_none());
	assert!(s.claim_batch(3, 4, 1024).is_some());
}

#[test]
fn batch_size_caps_the_claim() {
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 4);
	for f in 0..10 {
		s.submit(req(1, f, FramePriority::Playback));
	}
	let batch = s.claim_batch(0, 100, 1024).unwrap();
	assert_eq!(batch.frames.len(), 4, "batch size B caps the claim");
	// Ascending frame order inside the batch.
	let frames: Vec<i64> = batch.frames.iter().map(|f| f.key.frame).collect();
	assert_eq!(frames, vec![0, 1, 2, 3]);
}

#[test]
fn claim_batch_skips_requests_exceeding_slot_capacity() {
	// M15 S3 grow-on-demand: a request needing more bytes than the
	// worker's current slot capacity stays pending (the dispatcher can
	// grow the segment and claim it later); smaller requests still flow.
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 100);
	for f in 0..4 {
		let mut r = req(1, f, FramePriority::Playback);
		r.slot_bytes = if f == 1 { 33_000_000 } else { 8_300_000 };
		s.submit(r);
	}
	// max_bytes 8_300_000: frame 1 (33 MB) is skipped.
	let batch = s.claim_batch(0, 100, 8_300_000).unwrap();
	let frames: Vec<i64> = batch.frames.iter().map(|f| f.key.frame).collect();
	assert_eq!(frames, vec![0, 2, 3]);
	assert_eq!(s.pending_count(), 1, "the oversized frame stays pending");
	assert_eq!(
		s.max_pending_bytes_for_worker(0, 8_300_000),
		Some(33_000_000),
		"the dispatcher sees the growth need"
	);
	// After the segment grows, the oversized frame is claimable.
	let batch = s.claim_batch(0, 100, 33_000_000).unwrap();
	assert_eq!(batch.frames.len(), 1);
	assert_eq!(batch.frames[0].key.frame, 1);
	assert_eq!(s.pending_count(), 0);
}

#[test]
fn done_and_failed_drop_the_claim() {
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 4);
	s.submit(req(1, 0, FramePriority::Playback));
	s.submit(req(1, 1, FramePriority::Playback));
	let batch = s.claim_batch(0, 4, 1024).unwrap();
	assert_eq!(batch.frames.len(), 2);
	let k0 = batch.frames[0].key;
	let k1 = batch.frames[1].key;
	assert!(s.frame_done(&k0).is_some());
	assert!(s.frame_failed(&k1).is_some());
	assert_eq!(s.claimed_count(), 0);
	assert_eq!(
		s.pending_count(),
		0,
		"frame_failed is terminal (purple frame fallback)"
	);
	// Unknown keys are no-ops.
	assert!(s.frame_done(&k0).is_none());
}

#[test]
fn resubmit_of_claimed_key_is_rejected_until_done() {
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 4);
	let r = req(1, 5, FramePriority::Playback);
	let key = r.key;
	assert!(matches!(s.submit(r), SubmitOutcome::Accepted));
	let _ = s.claim_batch(0, 4, 1024).unwrap();
	// In flight: rejected.
	assert!(matches!(
		s.submit(req(1, 5, FramePriority::Seek)),
		SubmitOutcome::InFlight
	));
	s.frame_done(&key);
	// After completion the same key may be requested again (new
	// version in practice).
	assert!(matches!(
		s.submit(req(1, 5, FramePriority::Seek)),
		SubmitOutcome::Accepted
	));
}

#[test]
fn resubmit_replaces_pending_and_returns_the_old_request() {
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 4);
	let first = req(1, 5, FramePriority::Playback);
	let key = first.key;
	assert!(matches!(s.submit(first), SubmitOutcome::Accepted));
	// A second submission under the same key replaces the pending entry
	// and hands the superseded request back (the dispatcher cancels its
	// ticket completion with Error::State).
	let second = req(1, 5, FramePriority::Seek);
	let second_key = second.key;
	match s.submit(second) {
		SubmitOutcome::Replaced(old) => {
			assert_eq!(old.key, key);
			assert_eq!(old.priority, FramePriority::Playback);
		}
		other => panic!("expected Replaced, got {other:?}"),
	}
	let _ = s.claim_batch(0, 4, 1024).unwrap();
	assert_eq!(s.claimed_worker(&second_key), Some(0));
}

#[test]
fn cancel_sequence_drops_pending_and_claimed() {
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 4);
	for f in 0..6 {
		s.submit(req(7, f, FramePriority::Playback));
	}
	let _ = s.claim_batch(0, 4, 1024).unwrap(); // claims 3 of sequence 7 (reserve)
	s.submit(req(8, 0, FramePriority::Playback)); // other sequence
	let dropped = s.cancel_sequence(7);
	assert_eq!(dropped.len(), 6, "all 6 sequence-7 requests dropped");
	assert_eq!(s.pending_count(), 1, "sequence 8 untouched");
	assert_eq!(s.claimed_count(), 0);
}

#[test]
fn default_batch_size_is_120_over_workers() {
	let s: PreviewScheduler<u64> = PreviewScheduler::new(4, 0);
	assert_eq!(s.batch_size(), 30);
	let s: PreviewScheduler<u64> = PreviewScheduler::new(0, 0);
	assert_eq!(s.workers(), 1);
	assert_eq!(s.batch_size(), 120);
}

#[test]
fn unknown_worker_claims_nothing() {
	let mut s: PreviewScheduler<u64> = PreviewScheduler::new(2, 4);
	s.submit(req(1, 0, FramePriority::Playback));
	assert!(s.claim_batch(2, 4, 1024).is_none());
}
