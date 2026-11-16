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

//! The preview scheduler (M15 S1; design doc §3.2): frame-request
//! ordering, interleaved batch claims, crash re-dispatch and flow
//! control, as pure single-threaded logic.
//!
//! The scheduler knows nothing about processes or shared memory — it
//! turns a stream of [`FrameRequest`]s into [`ClaimedBatch`]es for the
//! [`crate::procpool::ProcessDispatcher`] to hand to workers:
//!
//!   - **Interleaved batch claims.** With `W` workers, the pending frame
//!     stream is sharded round-robin: worker `i` claims frames whose
//!     frame number is `≡ i (mod W)`, in batches of `B ≈ 120 / W`
//!     (configurable). Adjacent frame numbers therefore land on
//!     different workers and finish at nearly the same time; every
//!     frame belongs to exactly one worker (no work stealing).
//!   - **Priorities.** Seek/current frame > playback window (nearer the
//!     playhead first) > background (export/thumbnails). Within one
//!     priority class batches keep ascending frame order.
//!   - **Crash recovery.** A crashed worker's claimed frames (its whole
//!     un-started batches plus the un-finished frames of started ones)
//!     are re-queued and may be claimed by ANY healthy worker — crash
//!     re-dispatch is failure recovery, not stealing.
//!   - **Flow control.** [`PreviewScheduler::claim_batch`] never claims
//!     more frames than the caller's `credit` (the worker's free shm
//!     slot count): slots are the credit. Playback/Background claims
//!     leave one credit unused — the per-worker interactive reserve, so
//!     a window batch can never drain the last slot and starve a
//!     UI-blocking seek (window slots are released by UI-thread
//!     consumption; a seek waiting on them deadlocks the UI).
//!   - **Cancellation.** Frame keys carry a parameter `version`;
//!     submitting a newer version of a key invalidates the older one,
//!     and [`PreviewScheduler::cancel_sequence`] drops a whole sequence.
//!
//! All methods are non-blocking; the dispatcher drives them from its
//! poll loop (UI tick).

use std::collections::HashMap;
use std::mem;

/// A frame request key: `(sequence, frame number, parameter version)`.
/// The version covers graph/proxy/resolution/color-parameter changes —
/// bumping it invalidates outstanding requests for the same frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameKey {
	/// Sequence identity.
	pub sequence: u64,
	/// Frame number within the sequence.
	pub frame: i64,
	/// Parameter version (graph version / proxy tier / resolution tier /
	/// color).
	pub version: u64,
}

/// Frame request priority class (lower value = more urgent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum FramePriority {
	/// Seek / current playhead frame (single-frame insert, top priority).
	#[default]
	Seek,
	/// Playback window frames (ordered by [`FrameRequest::distance`]).
	Playback,
	/// Background work (export, thumbnails, full-res stills).
	Background,
}

/// One pending frame request. `payload` is opaque to the scheduler (the
/// dispatcher uses it to find the ticket's params + completion).
#[derive(Clone, Debug)]
pub struct FrameRequest<P> {
	/// The request key.
	pub key: FrameKey,
	/// Priority class.
	pub priority: FramePriority,
	/// Distance from the playhead in frames (orders the Playback class;
	/// unused by Seek/Background).
	pub distance: i64,
	/// Caller payload.
	pub payload: P,
	/// Slot bytes this request needs (frame size x wire format; M15 S3).
	/// `claim_batch` skips requests whose bytes exceed the worker's current
	/// slot capacity, so the dispatcher can grow the segment first (grow-
	/// on-demand geometry, design §3.1).
	pub slot_bytes: usize,
}

/// A batch of frames one worker claimed.
#[derive(Clone, Debug)]
pub struct ClaimedBatch<P> {
	/// Batch identity (unique per scheduler).
	pub batch_id: u64,
	/// The claiming worker index.
	pub worker: usize,
	/// The claimed frames in render order (priority class first, then
	/// ascending frame number).
	pub frames: Vec<FrameRequest<P>>,
}

struct Claim<P> {
	worker: usize,
	request: FrameRequest<P>,
}

struct PendingEntry<P> {
	request: FrameRequest<P>,
	/// True when any worker may claim this frame (crash re-dispatch);
	/// false when the interleaved shard rule applies.
	any_worker: bool,
}

/// The outcome of [`PreviewScheduler::submit`].
#[derive(Debug)]
pub enum SubmitOutcome<P> {
	/// A new request was accepted (nothing was pending under its key).
	Accepted,
	/// An existing pending request under the same key was replaced; the
	/// superseded request is returned so the dispatcher can cancel its
	/// ticket completion (M15 S2).
	Replaced(FrameRequest<P>),
	/// The key is already claimed (in flight); the request was rejected.
	InFlight,
}

/// The scheduler state machine (single-threaded by contract).
pub struct PreviewScheduler<P> {
	workers: usize,
	batch_size: usize,
	pending: Vec<PendingEntry<P>>,
	claimed: HashMap<FrameKey, Claim<P>>,
	next_batch_id: u64,
	/// Total frames re-queued by worker crashes (tests/metrics).
	crash_requeued: u64,
}

impl<P: Clone> PreviewScheduler<P> {
	/// Scheduler for `workers` workers (at least 1). `batch_size` 0 picks
	/// the design default `max(1, 120 / workers)`.
	pub fn new(workers: usize, batch_size: usize) -> Self {
		let workers = workers.max(1);
		let batch_size = if batch_size == 0 {
			(120 / workers).max(1)
		} else {
			batch_size
		};
		Self {
			workers,
			batch_size,
			pending: Vec::new(),
			claimed: HashMap::new(),
			next_batch_id: 1,
			crash_requeued: 0,
		}
	}

	/// The configured worker count.
	pub fn workers(&self) -> usize {
		self.workers
	}

	/// Change the worker count at run time (dynamic pool resize driven by
	/// the GPU-vram budget on resolution switches: 1080p can run the full
	/// CPU-bound pool, 4K only a vram-bounded fraction). The sharding
	/// modulus follows the new count immediately — pending frames are
	/// re-sharded on their next claim, so a shrink only reshuffles the
	/// not-yet-claimed queue (in-flight frames stay on their worker).
	/// `batch_size` is recomputed for the new count (the design figure
	/// `120 / W`).
	pub fn set_worker_count(&mut self, workers: usize) {
		let workers = workers.max(1);
		let old = mem::replace(&mut self.workers, workers);
		if old != workers {
			self.batch_size = (120 / workers).max(1).min(self.batch_size.max(1));
		}
	}

	/// The scheduler's current per-worker claim batch size.
	///
	/// [`PreviewScheduler::new`] derives it as `max(1, 120 / workers)` when 0 is
	/// passed; [`PreviewScheduler::set_worker_count`] lowers it to that figure on
	/// a pool resize (it never raises a smaller configured value). The batch
	/// actually claimed is further capped by the worker's credit and by
	/// `max_bytes` (see [`PreviewScheduler::claim_batch`]).
	pub fn batch_size(&self) -> usize {
		self.batch_size
	}

	/// Submit a frame request. An already-pending request with the same
	/// key is replaced; a key already claimed (in flight) is rejected —
	/// the dispatcher must cancel/re-version it first.
	///
	/// Seek-priority requests (interactive frame / real-time audio) are
	/// claimable by ANY worker, not just their interleaved shard: the
	/// no-stealing shard rule exists to keep ADJACENT PLAYBACK frames
	/// finishing together, but pinning a single urgent frame to one worker
	/// starves it whenever that worker's slots are all held by the window
	/// (the UI thread then waits on the seek while the window's slot
	/// releases run on that same thread — the seek-starvation deadlock).
	pub fn submit(&mut self, request: FrameRequest<P>) -> SubmitOutcome<P> {
		if self.claimed.contains_key(&request.key) {
			return SubmitOutcome::InFlight;
		}
		let any_worker = request.priority == FramePriority::Seek;
		if let Some(entry) = self
			.pending
			.iter_mut()
			.find(|e| e.request.key == request.key)
		{
			entry.any_worker = any_worker;
			let old = mem::replace(&mut entry.request, request);
			return SubmitOutcome::Replaced(old);
		}
		self.pending.push(PendingEntry {
			request,
			any_worker,
		});
		SubmitOutcome::Accepted
	}

	/// Whether `key` is currently claimed (in flight).
	///
	/// A claimed request belongs to one worker until it reports completion or the
	/// dispatcher cancels and re-versions it. Use
	/// [`PreviewScheduler::claimed_worker`] for the owning worker index.
	pub fn is_claimed(&self, key: &FrameKey) -> bool {
		self.claimed.contains_key(key)
	}

	/// Claim the next batch for `worker`: the worker's interleaved shard
	/// (frame number `≡ worker (mod W)`, plus crash-requeued frames and
	/// Seek-priority requests — both claimable by any worker), ordered by
	/// priority class / playhead distance / ascending frame, capped at
	/// `min(batch_size, credit)`. Playback/Background claims additionally
	/// leave one credit unused (the per-worker interactive reserve; see
	/// [`PreviewScheduler::submit`]). Requests needing more than
	/// `max_bytes` of slot space are skipped (they stay pending until the
	/// dispatcher grows the segment). Returns `None` when nothing
	/// claimable (`credit == 0`, unknown worker, empty shard, all
	/// oversized, or only the reserve remains).
	///
	/// Claimed Playback frames never go to another worker while in flight
	/// (no stealing).
	pub fn claim_batch(
		&mut self,
		worker: usize,
		credit: usize,
		max_bytes: usize,
	) -> Option<ClaimedBatch<P>> {
		if worker >= self.workers || credit == 0 {
			return None;
		}
		let workers = self.workers;
		let mut indexes: Vec<usize> = self
			.pending
			.iter()
			.enumerate()
			.filter(|(_, e)| {
				e.request.slot_bytes <= max_bytes
					&& (e.any_worker
						|| e.request.key.frame.rem_euclid(workers as i64) == worker as i64)
			})
			.map(|(i, _)| i)
			.collect();
		if indexes.is_empty() {
			return None;
		}
		indexes.sort_by(|&a, &b| {
			let ra = &self.pending[a].request;
			let rb = &self.pending[b].request;
			ra.priority
				.cmp(&rb.priority)
				.then(ra.distance.cmp(&rb.distance))
				.then(ra.key.frame.cmp(&rb.key.frame))
				.then(ra.key.sequence.cmp(&rb.key.sequence))
		});
		// Per-worker interactive reserve: Playback/Background claims must
		// leave one slot free. Window frames are released by UI-thread
		// consumption/eviction, so a batch that drains the worker's last
		// slot can starve a UI-blocking seek FOREVER; seeks and audio
		// (Seek priority) complete on the worker without UI involvement,
		// so they may use the last slot. (The global
		// `preview_window_capacity` reserve alone did not prevent this:
		// its accounting is pool-wide, while slot exhaustion happens per
		// worker.)
		let cap = self.batch_size.min(credit);
		let mut taken: Vec<usize> = Vec::with_capacity(cap);
		for &i in &indexes {
			if taken.len() >= cap {
				break;
			}
			let seek = self.pending[i].request.priority == FramePriority::Seek;
			if !seek && taken.len() + 1 >= credit {
				continue; // keep the reserve slot free
			}
			taken.push(i);
		}
		if taken.is_empty() {
			return None;
		}
		let indexes = taken;

		let batch_id = self.next_batch_id;
		self.next_batch_id += 1;

		// Collect claimed frames (removal order does not matter; the batch
		// keeps the sorted order).
		let mut frames: Vec<FrameRequest<P>> = Vec::with_capacity(indexes.len());
		let mut marked: Vec<bool> = vec![false; self.pending.len()];
		for &i in &indexes {
			marked[i] = true;
		}
		let mut kept: Vec<PendingEntry<P>> = Vec::with_capacity(self.pending.len() - indexes.len());
		for (i, entry) in self.pending.drain(..).enumerate() {
			if marked[i] {
				self.claimed.insert(
					entry.request.key,
					Claim {
						worker,
						request: entry.request.clone(),
					},
				);
				frames.push(entry.request);
			} else {
				kept.push(entry);
			}
		}
		self.pending = kept;
		frames.sort_by(|a, b| {
			a.priority
				.cmp(&b.priority)
				.then(a.distance.cmp(&b.distance))
				.then(a.key.frame.cmp(&b.key.frame))
		});
		Some(ClaimedBatch {
			batch_id,
			worker,
			frames,
		})
	}

	/// Report a claimed frame as rendered (frame_ready). Returns the
	/// request when the key was in flight.
	pub fn frame_done(&mut self, key: &FrameKey) -> Option<FrameRequest<P>> {
		self.claimed.remove(key).map(|c| c.request)
	}

	/// Pending-request count (dispatcher diagnostics).
	pub fn pending_len(&self) -> usize {
		self.pending.len()
	}

	/// One-line summary of the pending queue for starvation debugging
	/// (frame, pinned shard, needed slot bytes, any_worker).
	pub fn pending_summary(&self) -> Vec<(i64, i64, usize, bool)> {
		self.pending
			.iter()
			.map(|e| {
				(
					e.request.key.frame,
					e.request.key.frame.rem_euclid(self.workers as i64),
					e.request.slot_bytes,
					e.any_worker,
				)
			})
			.collect()
	}

	/// Report a claimed frame as permanently failed (frame_failed; the
	/// main process paints the fallback). The claim is dropped WITHOUT
	/// re-dispatch — a render error is not a crash. Returns the request
	/// when the key was in flight.
	pub fn frame_failed(&mut self, key: &FrameKey) -> Option<FrameRequest<P>> {
		self.claimed.remove(key).map(|c| c.request)
	}

	/// Re-queue every frame claimed by `worker` (crash recovery): its
	/// un-started batches and the un-finished frames of started batches
	/// all come back as pending, claimable by ANY healthy worker.
	/// Returns the re-queued requests.
	pub fn worker_crashed(&mut self, worker: usize) -> Vec<FrameRequest<P>> {
		let mut reclaimed = Vec::new();
		self.claimed.retain(|_, claim| {
			if claim.worker == worker {
				reclaimed.push(claim.request.clone());
				false
			} else {
				true
			}
		});
		for request in reclaimed.iter().cloned() {
			self.pending.push(PendingEntry {
				request,
				any_worker: true,
			});
		}
		self.crash_requeued += reclaimed.len() as u64;
		reclaimed
	}

	/// Cancel one key, pending OR claimed (single-frame cancellation;
	/// the dispatcher delivers the ticket's `Error::State` itself).
	/// Returns true when the key was known.
	pub fn cancel_key(&mut self, key: &FrameKey) -> bool {
		let before = self.pending.len();
		self.pending.retain(|e| &e.request.key != key);
		if self.pending.len() != before {
			return true;
		}
		self.claimed.remove(key).is_some()
	}

	/// Cancel every pending AND claimed frame of `sequence` (frame-key
	/// invalidation; the `cancel` wire message covers the worker side).
	/// Returns the dropped requests so the dispatcher can fire their
	/// completions with `Error::State`.
	pub fn cancel_sequence(&mut self, sequence: u64) -> Vec<FrameRequest<P>> {
		let mut dropped = Vec::new();
		self.pending.retain(|e| {
			if e.request.key.sequence == sequence {
				dropped.push(e.request.clone());
				false
			} else {
				true
			}
		});
		self.claimed.retain(|_, c| {
			if c.request.key.sequence == sequence {
				dropped.push(c.request.clone());
				false
			} else {
				true
			}
		});
		dropped
	}

	/// Pending (unclaimed) request count.
	pub fn pending_count(&self) -> usize {
		self.pending.len()
	}

	/// The largest `slot_bytes` among pending requests claimable by
	/// `worker` (its shard plus crash-requeued frames) that exceeds
	/// `current`, if any. The dispatcher uses this to grow a worker's
	/// segment before the next claim (M15 S3 grow-on-demand geometry).
	pub fn max_pending_bytes_for_worker(&self, worker: usize, current: usize) -> Option<usize> {
		if worker >= self.workers {
			return None;
		}
		let workers = self.workers;
		self.pending
			.iter()
			.filter(|e| {
				e.any_worker || e.request.key.frame.rem_euclid(workers as i64) == worker as i64
			})
			.map(|e| e.request.slot_bytes)
			.max()
			.filter(|&m| m > current)
	}

	/// Claimed (in-flight) request count.
	pub fn claimed_count(&self) -> usize {
		self.claimed.len()
	}

	/// The worker currently holding `key`, or `None` when it is not claimed.
	///
	/// The returned index is both the worker that claimed the batch and the shard
	/// the frame belongs to. Counterpart of [`PreviewScheduler::is_claimed`].
	pub fn claimed_worker(&self, key: &FrameKey) -> Option<usize> {
		self.claimed.get(key).map(|c| c.worker)
	}

	/// Total frames re-queued by crashes so far (tests/metrics).
	pub fn crash_requeued(&self) -> u64 {
		self.crash_requeued
	}
}

#[cfg(test)]
mod tests;
