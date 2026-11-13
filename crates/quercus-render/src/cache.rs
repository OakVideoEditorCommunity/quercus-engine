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

//! Playback caches: validated/requested range bookkeeping and the
//! on-disk frame hash cache (C++ `PlaybackCache`/`FrameHashCache`/
//! `AudioPlaybackCache`/`AudioWaveformCache`/`ThumbnailCache`).
//!
//! CPP-PARITY notes:
//! - The on-disk state file layout matches `BinaryStreamReader/Writer`
//!   (src/render/transition/binarystream.h) byte for byte: big-endian
//!   u32/i32 fields, 16 raw UUID bytes — C++ and Rust builds share the
//!   same `<cache_dir>/<uuid>/state` files.
//! - The frame filename scheme matches `FrameHashCache::cache_path_name`:
//!   `<cache_dir>/<uuid>/<timestamp>` with the timestamp computed via
//!   `Timecode::time_to_timestamp(…, k_round)`.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use quercus_core::{Rational, TimeRange, TimeRangeList};

use crate::error::{Error, Result};

/// Opaque identity of the owning node (the quercusnode NodeId identity
/// integer; the cache never calls back into node code — the C++
/// `parent_` back-pointer is reduced to this identity plus explicit
/// C ABI calls where unavoidable).
pub type OwnerIdentity = u64;

/// Cache flavor (the C++ subclasses become one type + kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheKind {
	/// Video frame hash cache (disk-backed).
	VideoFrame,
	/// Thumbnail cache (disk-backed, frame-hash family).
	Thumbnail,
	/// Audio playback cache.
	AudioPlayback,
	/// Audio waveform cache.
	AudioWaveform,
}

impl CacheKind {
	/// True for the frame-hash flavors (disk-backed, carry a timebase and
	/// frame filenames).
	pub fn is_frame_hash(self) -> bool {
		matches!(self, CacheKind::VideoFrame | CacheKind::Thumbnail)
	}
}

// ---- canonical UUID text ("{8-4-4-4-12}", lowercase) ----------------------

/// C++ `create_uuid_text()`: canonical "{8-4-4-4-12}" lowercase text with
/// version-4 and RFC-4122 variant bits.
pub fn create_uuid_text() -> String {
	uuid::Uuid::new_v4().braced().to_string()
}

// ---- binary stream helpers (QDataStream big-endian subset) -----------------

/// Appends `v` to `out` as 4 big-endian bytes.
///
/// `QDataStream` default byte order, used for the version and event tags at the
/// start of the shared state file; the buffer grows in place and nothing is
/// returned.
fn write_be_u32(out: &mut Vec<u8>, v: u32) {
	out.extend_from_slice(&v.to_be_bytes());
}

/// Appends `v` to `out` as 4 big-endian bytes.
///
/// `QDataStream` default byte order, used for the rational numerator/denominator
/// fields of the shared state file; the buffer grows in place and nothing is
/// returned.
fn write_be_i32(out: &mut Vec<u8>, v: i32) {
	out.extend_from_slice(&v.to_be_bytes());
}

/// 16 raw bytes in RFC 4122 order from the canonical text form.
///
/// Well-formed input goes through the `uuid` crate's parser; anything
/// else falls back to a lenient nibble scan (invalid characters count as
/// zero nibbles, anything past the 32nd nibble is ignored) for C++
/// QDataStream parity.
fn uuid_text_to_bytes(uuid: &str) -> [u8; 16] {
	if let Ok(parsed) = uuid::Uuid::parse_str(uuid) {
		return *parsed.as_bytes();
	}
	let mut bytes = [0u8; 16];
	let mut nibble = 0usize;
	for c in uuid.chars() {
		if c == '{' || c == '}' || c == '-' {
			continue;
		}
		if nibble >= 32 {
			break;
		}
		let d = c.to_digit(16).unwrap_or(0) as u8;
		if nibble.is_multiple_of(2) {
			bytes[nibble / 2] = d << 4;
		} else {
			bytes[nibble / 2] |= d;
		}
		nibble += 1;
	}
	bytes
}

/// Formats 16 raw UUID bytes as canonical lowercase "{8-4-4-4-12}" text.
///
/// `bytes` are in RFC 4122 / network order, the order stored in state files; the
/// braces and dashes are added by the `uuid` crate, matching C++
/// `create_uuid_text`. Counterpart of the crate-internal lenient text-to-bytes
/// parse used when loading state.
fn bytes_to_uuid_text(bytes: &[u8; 16]) -> String {
	uuid::Uuid::from_bytes(*bytes).braced().to_string()
}

/// Reader over a byte slice; reads past the end yield zeroed values
/// (QDataStream ReadPastEnd semantics).
///
/// Std covers the endian conversion (`from_be_bytes`); the reader itself
/// stays because neither std nor the usual third-party crates
/// (`byteorder` & co. error on short reads) provide QDataStream's
/// zero-fill-on-EOF contract, which the shared on-disk state format
/// relies on.
struct ByteReader<'a> {
	data: &'a [u8],
	pos: usize,
}

impl<'a> ByteReader<'a> {
	fn new(data: &'a [u8]) -> Self {
		Self { data, pos: 0 }
	}

	fn read_u32(&mut self) -> u32 {
		u32::from_be_bytes(self.read_array())
	}

	fn read_i32(&mut self) -> i32 {
		i32::from_be_bytes(self.read_array())
	}

	fn read_uuid(&mut self) -> [u8; 16] {
		self.read_array()
	}

	/// Next `N` bytes as a fixed array; short reads stay zero-padded on
	/// the right.
	fn read_array<const N: usize>(&mut self) -> [u8; N] {
		let mut b = [0u8; N];
		self.take(&mut b);
		b
	}

	/// Copy up to `out.len()` bytes into `out`, advancing the cursor;
	/// returns how many bytes were actually copied.
	fn take(&mut self, out: &mut [u8]) -> usize {
		let n = (self.data.len() - self.pos).min(out.len());
		out[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
		self.pos += n;
		n
	}
}

/// Modification time in milliseconds since the epoch (C++
/// `modification_time_msecs`; 0 when unknown).
fn modification_time_msecs(path: &std::path::Path) -> i64 {
	std::fs::metadata(path)
		.and_then(|m| m.modified())
		.ok()
		.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
		.map(|d| d.as_millis() as i64)
		.unwrap_or(0)
}

/// The unified cache: the C++ `PlaybackCache` / `FrameHashCache` /
/// `AudioPlaybackCache` / `AudioWaveformCache` / `ThumbnailCache`
/// subclass family as one type tagged by [`CacheKind`].
///
/// The cache tracks two sets of time ranges for its owner's output:
/// *validated* ranges (frames/samples already rendered and stored on
/// disk) and *requested* ranges (queued for rendering, used to avoid
/// scheduling the same work twice). `validate`/`invalidate` move ranges
/// between those sets and persist the bookkeeping to
/// `<disk_dir>/<uuid>/state` in the C++ byte format, so C++ and Rust
/// builds share cache directories. Frame-hash flavors additionally map
/// a time to its frame file `<disk_dir>/<uuid>/<timestamp>` via
/// [`timebase`](Self::timebase).
///
/// Passthroughs link ranges to *another* cache's uuid, letting a
/// passthrough node serve frames straight from its source's cache
/// directory without re-rendering.
///
/// Threading: the FFI layer serializes access through the public
/// [`lock`](Self::lock) (C++ `PlaybackCache::mutex()`); the methods
/// themselves take plain `&self`/`&mut self`.
pub struct PlaybackCache {
	/// Flavor; the frame-hash kinds are disk-backed and carry a timebase.
	pub kind: CacheKind,
	/// Identity of the owning node (from [`next_owner_identity`]); never
	/// dereferenced, only compared.
	pub owner: OwnerIdentity,
	/// Stable identity in canonical "{8-4-4-4-12}" text; names the
	/// on-disk subdirectory and is stored in project files.
	pub uuid: String,
	/// Frame timebase used to turn times into frame-file timestamps;
	/// `None` on audio caches (frame-hash flavors only).
	pub timebase: Option<Rational>,
	/// Ranges already rendered and cached on disk (normalized: merged,
	/// non-overlapping).
	validated: TimeRangeList,
	/// Ranges queued for rendering but not yet validated.
	requested: TimeRangeList,
	/// `(range, uuid)` links to another cache whose files cover that
	/// range; entries overlapping an invalidated range are pruned.
	passthroughs: Vec<(TimeRange, String)>,
	/// When false, `validate`/`invalidate` skip the disk write (tests
	/// and transient caches).
	saving_enabled: bool,
	/// Root disk cache directory (defaults to the process-wide default;
	/// project-owned caches re-point it through the node bridge later).
	disk_dir: String,
	/// mtime (ms) of the state file at the last load; unchanged files
	/// are not re-read.
	last_loaded_state: i64,
	/// External mutex exposed to the C ABI `quercusrender_cache_lock/unlock`
	/// (C++ `PlaybackCache::mutex()`; always pair the calls).
	pub lock: Mutex<()>,
}

impl PlaybackCache {
	/// Creates an empty cache of `kind` owned by `owner`.
	///
	/// `kind` selects the flavor (frame-hash kinds are disk-backed and time-based);
	/// `owner` is the owning node's identity, or a synthetic value from
	/// [`next_owner_identity`] — it is only compared, never dereferenced. The cache
	/// gets a fresh random v4 UUID text, no timebase (call
	/// [`PlaybackCache::set_timebase`] for frame-hash kinds), empty
	/// validated/requested/passthrough sets, saving enabled and the process-wide
	/// default disk cache root. Nothing is written to disk until a range is
	/// validated or a save runs.
	pub fn new(kind: CacheKind, owner: OwnerIdentity) -> Self {
		let disk_dir = quercus_core::commonutil::default_disk_cache_path();
		Self {
			kind,
			owner,
			uuid: create_uuid_text(),
			timebase: None,
			validated: TimeRangeList::new(),
			requested: TimeRangeList::new(),
			passthroughs: Vec::new(),
			saving_enabled: true,
			disk_dir,
			last_loaded_state: 0,
			lock: Mutex::new(()),
		}
	}

	/// The cache UUID text.
	pub fn uuid(&self) -> &str {
		&self.uuid
	}

	/// Replaces the cache UUID and reloads the on-disk state that belongs to it.
	///
	/// `uuid` is canonical "{8-4-4-4-12}" text stored as given (not validated) and
	/// selects the `<disk_dir>/<uuid>` subdirectory. The call then reads that
	/// directory's state file through [`PlaybackCache::load_state`]: a missing file
	/// clears the validated ranges and passthroughs, and read errors are ignored.
	pub fn set_uuid(&mut self, uuid: &str) {
		self.uuid = uuid.to_string();
		let dir = self.disk_dir.clone();
		let _ = self.load_state(std::path::Path::new(&dir));
	}

	/// Set the frame timebase (frame-hash flavors; C++ `set_timebase`).
	pub fn set_timebase(&mut self, tb: Rational) {
		self.timebase = Some(tb);
	}

	/// The frame timebase (null when unset).
	pub fn timebase(&self) -> Rational {
		self.timebase.unwrap_or(Rational::NULL)
	}

	/// Root disk cache directory.
	pub fn disk_dir(&self) -> &str {
		&self.disk_dir
	}

	/// Re-points the root disk cache directory used for state and frame files.
	///
	/// `dir` is stored verbatim — no validation, no directory creation; the UUID
	/// subdirectory is appended later by [`PlaybackCache::cache_directory`].
	/// Project-owned caches use this to keep their files under the project. State is
	/// not reloaded and existing links are not rewritten.
	pub fn set_disk_dir(&mut self, dir: &str) {
		self.disk_dir = dir.to_string();
	}

	/// Marks `range` as invalid, forcing it to be rendered again.
	///
	/// Removes `range` from the validated set (splitting a validated range that
	/// strictly contains it) and drops every passthrough link overlapping it, then
	/// persists the state file when saving is enabled; save errors are ignored. A
	/// zero-length range (`in == out`) is rejected with an `eprintln!` warning and
	/// changes nothing.
	pub fn invalidate(&mut self, range: TimeRange) {
		if range.in_() == range.out() {
			eprintln!("Tried to invalidate zero-length range");
			return;
		}
		self.validated.remove(range);
		self.passthroughs.retain(|(r, _)| !overlaps(*r, range));
		if self.saving_enabled {
			let dir = self.disk_dir.clone();
			let _ = self.save_state(std::path::Path::new(&dir));
		}
	}

	/// Marks `range` as validated, i.e. already rendered and stored on disk.
	///
	/// Inserts into the normalized validated set (merging overlapping and touching
	/// neighbours) and, when saving is enabled, persists the state file; save errors
	/// are ignored, so a failed write leaves the in-memory set updated. Unlike
	/// [`PlaybackCache::invalidate`], a zero-length range is accepted instead of
	/// being rejected.
	pub fn validate(&mut self, range: TimeRange) {
		self.validated.insert(range);
		if self.saving_enabled {
			let dir = self.disk_dir.clone();
			let _ = self.save_state(std::path::Path::new(&dir));
		}
	}

	/// True when any validated range exists (C++
	/// `has_validated_ranges`).
	pub fn has_validated_ranges(&self) -> bool {
		!self.validated.is_empty()
	}

	/// Returns the ranges already rendered and stored on disk.
	///
	/// The list is normalized (merged, non-overlapping) and drives frame-file
	/// lookup; treat it as read-only and move ranges through
	/// [`PlaybackCache::validate`] / [`PlaybackCache::invalidate`].
	pub fn validated_ranges(&self) -> &TimeRangeList {
		&self.validated
	}

	/// Returns the sub-ranges of `within` that still need rendering.
	///
	/// Starts from `within` clamped to `>= 0` on both ends and subtracts every
	/// validated range and every passthrough range, so an empty result means the
	/// whole request is already cached. The renderer uses this to schedule only the
	/// missing work.
	pub fn invalidated_ranges(&self, within: TimeRange) -> TimeRangeList {
		// Clamp to >= 0 (C++ does this for safety).
		let zero = Rational::new(0, 1);
		let mut in_ = within.in_();
		let mut out = within.out();
		if in_ < zero {
			in_ = zero;
		}
		if out < zero {
			out = zero;
		}
		let intersecting = TimeRange::new(in_, out);

		let mut invalidated = TimeRangeList::new();
		invalidated.insert(intersecting);

		for range in self.validated.ranges() {
			invalidated.remove(*range);
		}
		for (range, _) in &self.passthroughs {
			invalidated.remove(*range);
		}
		invalidated
	}

	/// Records `range` as requested (queued for rendering).
	///
	/// The request set is in-memory only and exists so the same work is not
	/// scheduled twice; entries are removed again with
	/// [`PlaybackCache::clear_request_range`] and the set is never persisted.
	pub fn request(&mut self, range: TimeRange) {
		self.requested.insert(range);
	}

	/// The requested-but-not-yet-validated ranges.
	pub fn requested_ranges(&self) -> &TimeRangeList {
		&self.requested
	}

	/// Removes `range` from the requested set so it is no longer queued.
	///
	/// Only the in-memory request bookkeeping changes: nothing is persisted and the
	/// validated set is untouched. Counterpart of [`PlaybackCache::request`], called
	/// when a queued render finishes or is cancelled.
	pub fn clear_request_range(&mut self, range: TimeRange) {
		self.requested.remove(range);
	}

	/// Links this cache to `other`, so `other`'s cached frames cover this cache.
	///
	/// Every validated range of `other`, plus every passthrough `other` already
	/// holds, is appended to this cache's passthroughs tagged with the corresponding
	/// source UUID; frame-hash flavors also adopt `other`'s timebase when it has
	/// one. The links are persisted when saving is enabled. Because only a
	/// `&PlaybackCache` is taken, the caller must ensure `other` is not this same
	/// cache — use [`PlaybackCache::set_passthrough_snapshot`] when the two handles
	/// may alias.
	pub fn set_passthrough(&mut self, other: &PlaybackCache) {
		for range in other.validated.ranges() {
			self.passthroughs.push((*range, other.uuid.clone()));
		}
		for (range, uuid) in &other.passthroughs {
			self.passthroughs.push((*range, uuid.clone()));
		}
		// FrameHashCache::set_passthrough also adopts the source timebase.
		if self.kind.is_frame_hash() {
			if let Some(tb) = other.timebase {
				self.timebase = Some(tb);
			}
		}
		if self.saving_enabled {
			let dir = self.disk_dir.clone();
			let _ = self.save_state(std::path::Path::new(&dir));
		}
	}

	/// Passthrough from a snapshot (avoids aliasing when the two handles
	/// may refer to the same cache).
	pub fn set_passthrough_snapshot(&mut self, snapshot: PassthroughSnapshot) {
		for range in snapshot.validated.ranges() {
			self.passthroughs.push((*range, snapshot.uuid.clone()));
		}
		for (range, uuid) in &snapshot.passthroughs {
			self.passthroughs.push((*range, uuid.clone()));
		}
		if self.kind.is_frame_hash() {
			if let Some(tb) = snapshot.timebase {
				self.timebase = Some(tb);
			}
		}
		if self.saving_enabled {
			let dir = self.disk_dir.clone();
			let _ = self.save_state(std::path::Path::new(&dir));
		}
	}

	/// Returns the passthrough links as `(range, uuid)` pairs.
	///
	/// Each entry says that `range` is served from the cache directory of the cache
	/// with that UUID, so this cache need not render it. Entries are appended by
	/// [`PlaybackCache::set_passthrough`] (and its snapshot variant) and pruned when
	/// they overlap an invalidated range.
	pub fn passthroughs(&self) -> &[(TimeRange, String)] {
		&self.passthroughs
	}

	/// Enables or disables persisting the range bookkeeping to disk.
	///
	/// With `enabled == false`, [`PlaybackCache::invalidate`],
	/// [`PlaybackCache::validate`] and [`PlaybackCache::set_passthrough`] skip their
	/// `save_state` call (transient caches and tests); the in-memory ranges still
	/// update. Defaults to `true`. Pairs with
	/// [`PlaybackCache::saving_enabled`].
	pub fn set_saving_enabled(&mut self, enabled: bool) {
		self.saving_enabled = enabled;
	}

	/// The persist toggle.
	pub fn saving_enabled(&self) -> bool {
		self.saving_enabled
	}

	/// Returns `<cache_dir>/<uuid>`, the directory holding this cache's state file
	/// and frame files.
	///
	/// `cache_dir` is the caller's root cache directory (not necessarily
	/// [`PlaybackCache::disk_dir`]); the path is only built, never created or
	/// written. Used by [`PlaybackCache::load_state`] and
	/// [`PlaybackCache::save_state`].
	pub fn cache_directory(&self, cache_dir: &std::path::Path) -> std::path::PathBuf {
		cache_dir.join(&self.uuid)
	}

	/// Disk state load (C++ `load_state`); `cache_dir` is the root cache
	/// directory. Missing state clears the ranges (C++ behavior).
	pub fn load_state(&mut self, cache_dir: &std::path::Path) -> Result<()> {
		let state_path = self.cache_directory(cache_dir).join("state");
		if !state_path.exists() {
			self.validated = TimeRangeList::new();
			self.passthroughs.clear();
			return Ok(());
		}

		let file_time = modification_time_msecs(&state_path);
		if file_time <= self.last_loaded_state {
			return Ok(());
		}

		let data = match std::fs::read(&state_path) {
			Ok(d) => d,
			Err(e) => return Err(Error::Failed(format!("read state: {e}"))),
		};
		let mut r = ByteReader::new(&data);

		let version = r.read_u32();
		if self.kind.is_frame_hash() {
			// FrameHashCache::LoadStateEvent
			let event_version = r.read_u32();
			if event_version == 1 {
				let num = r.read_i32();
				let den = r.read_i32();
				if num > 0 && den > 0 {
					self.timebase = Some(Rational::new(num as i64, den as i64));
				}
			}
		}

		if version == 1 {
			let valid_count = r.read_i32();
			for _ in 0..valid_count {
				let in_num = r.read_i32() as i64;
				let in_den = r.read_i32() as i64;
				let out_num = r.read_i32() as i64;
				let out_den = r.read_i32() as i64;
				self.validated.insert(TimeRange::new(
					Rational::new(in_num, in_den),
					Rational::new(out_num, out_den),
				));
			}

			let pass_count = r.read_i32();
			for _ in 0..pass_count {
				let in_num = r.read_i32() as i64;
				let in_den = r.read_i32() as i64;
				let out_num = r.read_i32() as i64;
				let out_den = r.read_i32() as i64;
				let uuid = bytes_to_uuid_text(&r.read_uuid());
				self.passthroughs.push((
					TimeRange::new(
						Rational::new(in_num, in_den),
						Rational::new(out_num, out_den),
					),
					uuid,
				));
			}
		}

		self.last_loaded_state = file_time;
		Ok(())
	}

	/// Writes this cache's bookkeeping to `<cache_dir>/<uuid>/state` in the C++
	/// `BinaryStreamWriter` format (big-endian version/event tags, counts, rational
	/// in/out pairs and raw 16-byte UUIDs), creating the directory as needed.
	///
	/// With no validated ranges and no passthroughs the state file is deleted
	/// instead of written, so an empty cache leaves nothing on disk. Frame-hash
	/// flavors additionally write the timebase (1/1 when unset). After a successful
	/// write the process-wide disk-cache quota is enforced (debounced and
	/// best-effort). Returns `Err` when the directory cannot be created or the file
	/// cannot be created/written/flushed; the in-memory state is unaffected either
	/// way. Counterpart of [`PlaybackCache::load_state`].
	pub fn save_state(&self, cache_dir: &std::path::Path) -> Result<()> {
		let dir = self.cache_directory(cache_dir);
		let state_path = dir.join("state");

		if self.validated.is_empty() && self.passthroughs.is_empty() {
			let _ = std::fs::remove_file(&state_path);
			return Ok(());
		}

		std::fs::create_dir_all(&dir)
			.map_err(|e| Error::Failed(format!("create cache dir: {e}")))?;

		let mut out = Vec::new();
		write_be_u32(&mut out, 1); // PlaybackCache version
		if self.kind.is_frame_hash() {
			// FrameHashCache::SaveStateEvent
			write_be_u32(&mut out, 1);
			let tb = self.timebase.unwrap_or(Rational::new(1, 1));
			write_be_i32(&mut out, tb.numerator() as i32);
			write_be_i32(&mut out, tb.denominator() as i32);
		}

		write_be_i32(&mut out, self.validated.ranges().len() as i32);
		for range in self.validated.ranges() {
			write_be_i32(&mut out, range.in_().numerator() as i32);
			write_be_i32(&mut out, range.in_().denominator() as i32);
			write_be_i32(&mut out, range.out().numerator() as i32);
			write_be_i32(&mut out, range.out().denominator() as i32);
		}

		write_be_i32(&mut out, self.passthroughs.len() as i32);
		for (range, uuid) in &self.passthroughs {
			write_be_i32(&mut out, range.in_().numerator() as i32);
			write_be_i32(&mut out, range.in_().denominator() as i32);
			write_be_i32(&mut out, range.out().numerator() as i32);
			write_be_i32(&mut out, range.out().denominator() as i32);
			out.extend_from_slice(&uuid_text_to_bytes(uuid));
		}

		let mut file = std::fs::File::create(&state_path)
			.map_err(|e| Error::Failed(format!("create state: {e}")))?;
		file.write_all(&out)
			.map_err(|e| Error::Failed(format!("write state: {e}")))?;
		file.flush()
			.map_err(|e| Error::Failed(format!("flush state: {e}")))?;
		// The state file counts toward the disk-cache budget; enforce it
		// after the write (debounced, best-effort, and lock-free).
		crate::manager::enforce_disk_cache_quota();
		Ok(())
	}

	/// The on-disk filename for a frame time (C++
	/// `FrameHashCache::get_valid_cache_filename`). `None` when the frame is
	/// not cached and no passthrough covers the time.
	pub fn frame_filename(&self, time: Rational) -> Option<String> {
		if !self.kind.is_frame_hash() {
			return None;
		}
		if is_cached_at(&self.validated, time) {
			return Some(self.cache_path_name(time, &self.uuid));
		}
		for (range, uuid) in &self.passthroughs {
			if range.contains(time) {
				return Some(self.cache_path_name(time, uuid));
			}
		}
		None
	}

	/// `cache_path_name(time)`: `<disk_dir>/<uuid>/<timestamp>` where the
	/// timestamp is `time_to_timestamp(time, tb, k_round)`.
	fn cache_path_name(&self, time: Rational, uuid: &str) -> String {
		let timestamp = match self.timebase {
			Some(tb) => tb.time_to_timestamp(time),
			// No valid timebase: whole seconds.
			None => time.to_f64().round() as i64,
		};
		std::path::Path::new(&self.disk_dir)
			.join(uuid)
			.join(timestamp.to_string())
			.to_string_lossy()
			.into_owned()
	}

	/// The static `cache_path_name(cache_path, cache_id, time, tb)`
	/// variant used by the FFI frame-cache load/save exports.
	pub fn frame_cache_path(
		cache_path: &str,
		cache_id: &str,
		time: Rational,
		timebase: Rational,
	) -> String {
		let timestamp = timebase.time_to_timestamp(time);
		std::path::Path::new(cache_path)
			.join(cache_id)
			.join(timestamp.to_string())
			.to_string_lossy()
			.into_owned()
	}
}

/// True when the two half-open ranges `a` and `b` intersect.
///
/// Ranges are half-open (`in` inclusive, `out` exclusive), the C++ `TimeRange`
/// default inclusivity, so ranges that merely touch (`a.out == b.in`) do not
/// overlap. Used to prune passthrough links that cover an invalidated range.
fn overlaps(a: TimeRange, b: TimeRange) -> bool {
	!(b.out() <= a.in_() || b.in_() >= a.out())
}

/// An owned snapshot of another cache's passthrough-relevant data
/// (validated ranges, passthroughs, timebase, uuid) so `set_passthrough`
/// cannot alias.
#[derive(Clone, Debug)]
pub struct PassthroughSnapshot {
	/// The source's validated ranges.
	pub validated: TimeRangeList,
	/// The source's passthroughs.
	pub passthroughs: Vec<(TimeRange, String)>,
	/// The source's timebase.
	pub timebase: Option<Rational>,
	/// The source's uuid.
	pub uuid: String,
}

/// True when `t` lies in any range (C++ `TimeRangeList::contains(Rational)`).
fn is_cached_at(list: &TimeRangeList, t: Rational) -> bool {
	list.ranges().iter().any(|r| r.contains(t))
}

/// Monotonic identity counter for caches without a real node identity.
static NEXT_CACHE_ID: AtomicU64 = AtomicU64::new(1);

/// A synthetic owner identity for detached caches (never collides with
/// node identities, which are pointer values).
pub fn next_owner_identity() -> OwnerIdentity {
	NEXT_CACHE_ID.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn tb_cache() -> PlaybackCache {
		let mut c = PlaybackCache::new(CacheKind::VideoFrame, 1);
		c.set_timebase(Rational::new(1, 30));
		c.set_saving_enabled(false);
		c
	}

	#[test]
	fn uuid_is_canonical_v4() {
		let u = create_uuid_text();
		assert_eq!(u.len(), 38);
		assert!(u.starts_with('{') && u.ends_with('}'));
		assert_eq!(u.chars().filter(|&c| c == '-').count(), 4);
		// version nibble at position 15 ("-4...").
		let b = u.as_bytes();
		assert_eq!(b[15], b'4');
		// variant nibble at position 20.
		let v = (b[20] as char).to_digit(16).unwrap();
		assert!(v == 8 || v == 9 || v == 10 || v == 11);
	}

	#[test]
	fn uuid_text_bytes_roundtrip() {
		let u = create_uuid_text();
		let b = uuid_text_to_bytes(&u);
		assert_eq!(bytes_to_uuid_text(&b), u);
	}

	#[test]
	fn invalidate_validate_roundtrip() {
		let mut c = tb_cache();
		let r = TimeRange::new(Rational::new(0, 1), Rational::new(10, 1));
		c.validate(r);
		assert!(c.has_validated_ranges());
		assert_eq!(c.invalidated_ranges(r).ranges().len(), 0);
		c.invalidate(TimeRange::new(Rational::new(4, 1), Rational::new(6, 1)));
		// C++ semantics: invalidated = intersecting − validated − passthrough.
		let inv = c.invalidated_ranges(r);
		assert_eq!(inv.ranges().len(), 1);
		assert_eq!(inv.ranges()[0].in_(), Rational::new(4, 1));
		assert_eq!(inv.ranges()[0].out(), Rational::new(6, 1));
		// The validated list still covers the two surviving sub-ranges.
		assert_eq!(c.validated_ranges().ranges().len(), 2);
	}

	#[test]
	fn zero_length_invalidate_is_rejected() {
		let mut c = tb_cache();
		let r = TimeRange::new(Rational::new(0, 1), Rational::new(5, 1));
		c.validate(r);
		c.invalidate(TimeRange::new(Rational::new(2, 1), Rational::new(2, 1)));
		assert!(
			c.has_validated_ranges(),
			"zero-length invalidate is a no-op"
		);
	}

	#[test]
	fn invalidated_ranges_clamps_below_zero() {
		let mut c = tb_cache();
		c.validate(TimeRange::new(Rational::new(0, 1), Rational::new(5, 1)));
		let inv = c.invalidated_ranges(TimeRange::new(Rational::new(-5, 1), Rational::new(10, 1)));
		// Only [5,10) remains after the clamp + validation removal.
		assert_eq!(inv.ranges().len(), 1);
		assert_eq!(inv.ranges()[0].in_(), Rational::new(5, 1));
		assert_eq!(inv.ranges()[0].out(), Rational::new(10, 1));
	}

	#[test]
	fn passthrough_excludes_ranges() {
		let mut a = tb_cache();
		let mut b = PlaybackCache::new(CacheKind::VideoFrame, 2);
		b.set_timebase(Rational::new(1, 30));
		b.set_saving_enabled(false);
		b.validate(TimeRange::new(Rational::new(0, 1), Rational::new(5, 1)));

		a.set_passthrough(&b);
		assert_eq!(a.passthroughs().len(), 1);
		assert_eq!(a.passthroughs()[0].1, b.uuid);

		let inv = a.invalidated_ranges(TimeRange::new(Rational::new(0, 1), Rational::new(10, 1)));
		assert_eq!(inv.ranges().len(), 1);
		assert_eq!(inv.ranges()[0].in_(), Rational::new(5, 1));

		// Invalidate a range overlapping the passthrough: it is unlinked.
		a.invalidate(TimeRange::new(Rational::new(2, 1), Rational::new(8, 1)));
		assert!(a.passthroughs().is_empty());
	}

	#[test]
	fn frame_filename_parity_scheme() {
		let mut c = tb_cache();
		c.set_uuid("{01234567-89ab-cdef-0123-456789abcdef}");
		let time = Rational::new(1, 2); // 0.5 s at 30fps → frame 15
		c.validate(TimeRange::new(time, time + Rational::new(1, 30)));
		let dir = std::env::temp_dir();
		c.set_disk_dir(&dir.to_string_lossy());
		let name = c.frame_filename(time).unwrap();
		assert_eq!(
			name,
			dir.join("{01234567-89ab-cdef-0123-456789abcdef}")
				.join("15")
				.to_string_lossy()
		);
	}

	#[test]
	fn frame_filename_none_when_not_cached() {
		let c = tb_cache();
		assert!(c.frame_filename(Rational::new(1, 30)).is_none());
	}

	#[test]
	fn audio_kind_has_no_frame_filename() {
		let c = PlaybackCache::new(CacheKind::AudioPlayback, 1);
		assert!(c.frame_filename(Rational::new(1, 1)).is_none());
	}

	#[test]
	fn request_ranges_and_clear() {
		let mut c = tb_cache();
		let r = TimeRange::new(Rational::new(0, 1), Rational::new(5, 1));
		c.request(r);
		assert_eq!(c.requested_ranges().ranges().len(), 1);
		c.request(TimeRange::new(Rational::new(3, 1), Rational::new(8, 1)));
		assert_eq!(c.requested_ranges().ranges().len(), 1, "merges on insert");
		assert_eq!(c.requested_ranges().ranges()[0].in_(), Rational::new(0, 1));
		c.clear_request_range(TimeRange::new(Rational::new(2, 1), Rational::new(3, 1)));
		assert_eq!(c.requested_ranges().ranges().len(), 2, "splits on clear");
	}

	#[test]
	fn audio_cache_disk_state_uses_base_format() {
		let dir =
			std::env::temp_dir().join(format!("quercusrender-audio-test-{}", next_owner_identity()));
		std::fs::create_dir_all(&dir).unwrap();
		let mut c = PlaybackCache::new(CacheKind::AudioPlayback, 1);
		c.set_saving_enabled(false);
		c.validate(TimeRange::new(Rational::new(0, 1), Rational::new(2, 1)));
		c.save_state(&dir).unwrap();
		// Base format: no timebase block (audio has no frame-hash event).
		let bytes = std::fs::read(dir.join(&c.uuid).join("state")).unwrap();
		let mut expect = Vec::new();
		write_be_u32(&mut expect, 1); // version
		write_be_i32(&mut expect, 1); // valid_count
		write_be_i32(&mut expect, 0);
		write_be_i32(&mut expect, 1);
		write_be_i32(&mut expect, 2);
		write_be_i32(&mut expect, 1);
		write_be_i32(&mut expect, 0); // pass_count
		assert_eq!(bytes, expect);
		// frame_filename is not available for audio kinds.
		assert!(c.frame_filename(Rational::new(0, 1)).is_none());
		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn cache_directory_helper() {
		let c = tb_cache();
		let dir = std::path::Path::new("/tmp/cache");
		assert_eq!(c.cache_directory(dir), dir.join(&c.uuid));
	}

	#[test]
	fn disk_state_roundtrip_binary_parity() {
		let dir = std::env::temp_dir().join(format!("quercusrender-test-{}", next_owner_identity()));
		std::fs::create_dir_all(&dir).unwrap();

		let mut c = tb_cache();
		c.set_timebase(Rational::new(1, 30));
		c.set_saving_enabled(true);
		c.validate(TimeRange::new(Rational::new(0, 1), Rational::new(10, 1)));
		c.validate(TimeRange::new(Rational::new(20, 1), Rational::new(30, 1)));
		c.save_state(&dir).unwrap();

		// Byte layout: ver=1, ver2=1, tb=1/30, count=2, 2×4×i32, count=0.
		let bytes = std::fs::read(dir.join(&c.uuid).join("state")).unwrap();
		let mut expect = Vec::new();
		write_be_u32(&mut expect, 1);
		write_be_u32(&mut expect, 1);
		write_be_i32(&mut expect, 1);
		write_be_i32(&mut expect, 30);
		write_be_i32(&mut expect, 2);
		for (i, o) in [(0i32, 10i32), (20, 30)] {
			write_be_i32(&mut expect, i);
			write_be_i32(&mut expect, 1);
			write_be_i32(&mut expect, o);
			write_be_i32(&mut expect, 1);
		}
		write_be_i32(&mut expect, 0);
		assert_eq!(bytes, expect, "C++ binary state layout parity");

		let mut c2 = PlaybackCache::new(CacheKind::VideoFrame, 99);
		c2.set_uuid(&c.uuid.clone());
		c2.set_timebase(Rational::new(1, 30));
		c2.set_saving_enabled(false);
		c2.load_state(&dir).unwrap();
		assert_eq!(c2.validated.ranges().len(), 2);
		assert_eq!(
			c2.validated.ranges()[0],
			TimeRange::new(Rational::new(0, 1), Rational::new(10, 1))
		);
		assert_eq!(
			c2.validated.ranges()[1],
			TimeRange::new(Rational::new(20, 1), Rational::new(30, 1))
		);

		// Re-save from the loaded cache → identical bytes.
		c2.set_saving_enabled(true);
		c2.save_state(&dir).unwrap();
		let bytes2 = std::fs::read(dir.join(&c2.uuid).join("state")).unwrap();
		assert_eq!(bytes, bytes2);

		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn save_state_removes_file_when_empty() {
		let dir = std::env::temp_dir().join(format!("quercusrender-test-{}", next_owner_identity()));
		std::fs::create_dir_all(&dir).unwrap();
		let mut c = tb_cache();
		c.set_saving_enabled(true);
		c.set_disk_dir(&dir.to_string_lossy());
		c.validate(TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)));
		c.save_state(&dir).unwrap();
		assert!(dir.join(&c.uuid).join("state").exists());
		c.invalidate(TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)));
		assert!(!dir.join(&c.uuid).join("state").exists());
		std::fs::remove_dir_all(&dir).ok();
	}

	// ---- remaining boundary surface --------------------------------------

	fn work_dir(tag: &str) -> std::path::PathBuf {
		let dir = std::env::temp_dir().join(format!("quercusrender-test-{tag}-{}", next_owner_identity()));
		std::fs::create_dir_all(&dir).unwrap();
		dir
	}

	#[test]
	fn accessors_and_null_timebase_defaults() {
		let mut c = PlaybackCache::new(CacheKind::AudioPlayback, 42);
		assert_eq!(c.uuid().len(), 38);
		assert_eq!(c.timebase(), Rational::NULL);
		assert!(c.saving_enabled());
		assert!(!c.disk_dir().is_empty());
		assert!(c.requested_ranges().is_empty());
		assert!(c.passthroughs().is_empty());

		c.set_timebase(Rational::new(1, 25));
		assert_eq!(c.timebase(), Rational::new(1, 25));
		c.set_saving_enabled(false);
		assert!(!c.saving_enabled());
		c.set_disk_dir("/tmp/quercus-cache-test");
		assert_eq!(c.disk_dir(), "/tmp/quercus-cache-test");
		c.set_uuid("{00000000-0000-4000-8000-000000000000}");
		assert_eq!(c.uuid(), "{00000000-0000-4000-8000-000000000000}");
	}

	#[test]
	fn request_and_clear_ranges() {
		let mut c = PlaybackCache::new(CacheKind::VideoFrame, 1);
		c.set_saving_enabled(false);
		let range = TimeRange::new(Rational::new(1, 1), Rational::new(2, 1));
		c.request(range);
		assert_eq!(c.requested_ranges().ranges(), &[range]);
		c.clear_request_range(range);
		assert!(c.requested_ranges().is_empty());
	}

	#[test]
	fn byte_reader_reads_past_the_end_as_zero() {
		let data = [0x12u8, 0x34, 0x56, 0x78];
		let mut r = ByteReader::new(&data);
		assert_eq!(r.read_u32(), 0x1234_5678);
		// Past the end: zeroed values, no panic.
		assert_eq!(r.read_u32(), 0);
		assert_eq!(r.read_i32(), 0);
		assert_eq!(r.read_uuid(), [0u8; 16]);
		let mut out = [0xFFu8; 4];
		assert_eq!(r.take(&mut out), 0);
		assert_eq!(out, [0xFFu8; 4]);

		// Partial reads copy only the bytes that exist.
		let mut r = ByteReader::new(&data);
		let mut short = [0u8; 2];
		assert_eq!(r.take(&mut short), 2);
		assert_eq!(short, [0x12, 0x34]);
		// A partial u32 read is left-aligned and zero-padded on the right.
		assert_eq!(r.read_u32(), 0x5678_0000);
	}

	#[test]
	fn uuid_text_conversion_ignores_trailing_nibbles() {
		let canonical = "{00112233-4455-6677-8899-aabbccddeeff}";
		let bytes = uuid_text_to_bytes(canonical);
		assert_eq!(
			bytes,
			[
				0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
				0xdd, 0xee, 0xff
			]
		);
		assert_eq!(bytes_to_uuid_text(&bytes), canonical);
		// Anything past the 32nd nibble is ignored (QDataStream parity).
		assert_eq!(uuid_text_to_bytes(&format!("{canonical}ffff")), bytes);
	}

	#[test]
	fn modification_time_is_zero_for_missing_paths() {
		assert_eq!(
			modification_time_msecs(std::path::Path::new("/definitely/not/here")),
			0
		);
	}

	#[test]
	fn set_uuid_reloads_the_disk_state() {
		let dir = work_dir("uuid-reload");
		let mut source = tb_cache();
		source.set_saving_enabled(true);
		source.set_disk_dir(&dir.to_string_lossy());
		let uuid = source.uuid.clone();
		source.validate(TimeRange::new(Rational::new(3, 1), Rational::new(9, 1)));
		source.save_state(&dir).unwrap();

		let mut target = PlaybackCache::new(CacheKind::VideoFrame, 2);
		target.set_saving_enabled(false);
		target.set_disk_dir(&dir.to_string_lossy());
		target.set_uuid(&uuid);
		assert_eq!(
			target.validated_ranges().ranges(),
			&[TimeRange::new(Rational::new(3, 1), Rational::new(9, 1))]
		);

		// A uuid without a state file clears the ranges (missing state).
		target.set_uuid("{00000000-0000-4000-8000-000000000000}");
		assert!(!target.has_validated_ranges());

		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn load_state_loads_a_clean_cache_and_skips_unchanged_files() {
		let dir = work_dir("load-skip");

		// One validated range persisted as `<dir>/<uuid>/state` by a
		// separate producer instance.
		let mut source = tb_cache();
		source.set_saving_enabled(true);
		source.set_disk_dir(&dir.to_string_lossy());
		let loaded = TimeRange::new(Rational::new(3, 1), Rational::new(9, 1));
		source.validate(loaded);
		source.save_state(&dir).unwrap();

		// The consumer starts with genuinely empty in-memory ranges (and a
		// zero `last_loaded_state`): the state has to come from the file.
		// `validate` is deliberately not called on this instance — that is
		// what made the old construction isomorphic. A no-op load or an
		// inverted mtime guard now leaves the assertions below failing.
		let mut target = PlaybackCache::new(CacheKind::VideoFrame, 2);
		target.set_saving_enabled(false);
		target.set_disk_dir(&dir.to_string_lossy());
		target.uuid = source.uuid.clone();
		assert!(
			target.validated_ranges().is_empty(),
			"the consumer starts with no ranges"
		);

		target.load_state(&dir).unwrap();
		assert_eq!(
			target.validated_ranges().ranges(),
			&[loaded],
			"the state file loads into empty memory"
		);
		assert_ne!(
			target.last_loaded_state, 0,
			"the load records the state file's mtime"
		);

		// Drop the memory only, then load the unchanged file again: the
		// mtime guard must skip it, so the range stays gone. A guard that
		// was removed would resurrect the range here (and an inverted one
		// would already have failed the load above).
		target.validated = TimeRangeList::new();
		target.load_state(&dir).unwrap();
		assert!(
			target.validated_ranges().is_empty(),
			"an unchanged state file is not reloaded"
		);

		// Force the reload path (a rewrite within the same millisecond
		// would make the mtime comparison flaky): the now-larger file is
		// re-read in full.
		source.validate(TimeRange::new(Rational::new(10, 1), Rational::new(11, 1)));
		source.save_state(&dir).unwrap();
		target.last_loaded_state = 0;
		target.load_state(&dir).unwrap();
		assert_eq!(
			target.validated_ranges().ranges(),
			source.validated_ranges().ranges(),
			"a forced load re-reads the whole state"
		);

		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn save_state_reports_directory_creation_errors() {
		let dir = work_dir("save-error");
		let blocker = dir.join("not-a-dir");
		std::fs::write(&blocker, b"file").unwrap();
		let mut c = tb_cache();
		c.set_saving_enabled(false);
		c.set_disk_dir(&blocker.to_string_lossy());
		c.validate(TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)));
		let err = c.save_state(&blocker).unwrap_err();
		assert!(format!("{err}").contains("create cache dir"), "{err}");
		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn passthrough_snapshot_links_ranges_without_aliasing() {
		let mut source = PlaybackCache::new(CacheKind::VideoFrame, 3);
		source.set_saving_enabled(false);
		source.set_timebase(Rational::new(1, 24));
		source.validate(TimeRange::new(Rational::new(2, 1), Rational::new(3, 1)));
		let snapshot = PassthroughSnapshot {
			validated: source.validated.clone(),
			passthroughs: vec![(
				TimeRange::new(Rational::new(8, 1), Rational::new(9, 1)),
				source.uuid.clone(),
			)],
			timebase: source.timebase,
			uuid: source.uuid.clone(),
		};

		let mut target = PlaybackCache::new(CacheKind::VideoFrame, 4);
		target.set_saving_enabled(false);
		target.set_passthrough_snapshot(snapshot.clone());
		assert_eq!(target.timebase(), Rational::new(1, 24));
		assert_eq!(
			target.passthroughs().len(),
			2,
			"validated source range plus the explicit entry"
		);
		// Passthroughs cover exactly the linked ranges; the gap between them
		// is still reported as invalidated.
		let inv = target.invalidated_ranges(TimeRange::new(Rational::new(2, 1), Rational::new(9, 1)));
		assert_eq!(
			inv.ranges(),
			&[TimeRange::new(Rational::new(3, 1), Rational::new(8, 1))]
		);

		// Audio caches keep their own timebase (frame-hash-only adoption).
		let mut audio = PlaybackCache::new(CacheKind::AudioPlayback, 5);
		audio.set_saving_enabled(false);
		audio.set_passthrough_snapshot(snapshot);
		assert_eq!(audio.timebase(), Rational::NULL);
	}

	#[test]
	fn passthrough_with_saving_enabled_persists_the_state() {
		let dir = work_dir("passthrough-save");
		let mut source = PlaybackCache::new(CacheKind::VideoFrame, 6);
		source.set_saving_enabled(false);
		source.validate(TimeRange::new(Rational::new(0, 1), Rational::new(2, 1)));

		let mut target = PlaybackCache::new(CacheKind::VideoFrame, 7);
		target.set_disk_dir(&dir.to_string_lossy());
		target.set_passthrough(&source);
		let state = dir.join(&target.uuid).join("state");
		assert!(state.exists(), "set_passthrough persists when saving is on");

		// Snapshot variant takes the same saving path.
		let snapshot = PassthroughSnapshot {
			validated: source.validated.clone(),
			passthroughs: Vec::new(),
			timebase: None,
			uuid: source.uuid.clone(),
		};
		target.set_passthrough_snapshot(snapshot);
		assert!(state.exists());
		assert_eq!(target.passthroughs().len(), 2);

		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn frame_paths_use_the_timebase_or_whole_seconds() {
		// Instance path with a timebase: 15s at 1/30 → frame 450.
		let mut with_tb = tb_cache();
		with_tb.validate(TimeRange::new(Rational::new(0, 1), Rational::new(16, 1)));
		let name = with_tb.frame_filename(Rational::new(15, 1)).expect("cached");
		assert_eq!(
			std::path::Path::new(&name)
				.file_name()
				.and_then(|n| n.to_str()),
			Some("450"),
			"{name}"
		);

		let path = PlaybackCache::frame_cache_path(
			"/cache",
			"id",
			Rational::new(15, 1),
			Rational::new(1, 30),
		);
		assert_eq!(
			path,
			std::path::Path::new("/cache")
				.join("id")
				.join("450")
				.to_string_lossy()
		);
		assert_eq!(
			PlaybackCache::frame_cache_path(
				"/cache",
				"id",
				Rational::new(1, 10),
				Rational::new(1, 1)
			),
			std::path::Path::new("/cache")
				.join("id")
				.join("0")
				.to_string_lossy()
		);

		// No timebase: whole seconds (round-half-away-from-zero).
		let mut c = PlaybackCache::new(CacheKind::VideoFrame, 8);
		c.set_saving_enabled(false);
		c.validate(TimeRange::new(Rational::new(0, 1), Rational::new(4, 1)));
		let name = c.frame_filename(Rational::new(1, 2)).expect("cached");
		assert_eq!(
			std::path::Path::new(&name)
				.file_name()
				.and_then(|n| n.to_str()),
			Some("1"),
			"{name}"
		);
		assert!(name.contains(c.uuid()), "{name}");
	}

	#[test]
	fn next_owner_identity_is_monotonic() {
		let a = next_owner_identity();
		let b = next_owner_identity();
		assert!(b > a);
	}
}
