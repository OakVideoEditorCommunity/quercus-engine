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
	let dir = std::env::temp_dir().join(format!(
		"quercusrender-audio-test-{}",
		next_owner_identity()
	));
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
	let dir = std::env::temp_dir().join(format!(
		"quercusrender-test-{tag}-{}",
		next_owner_identity()
	));
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
			0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
			0xee, 0xff
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
	let name = with_tb
		.frame_filename(Rational::new(15, 1))
		.expect("cached");
	assert_eq!(
		std::path::Path::new(&name)
			.file_name()
			.and_then(|n| n.to_str()),
		Some("450"),
		"{name}"
	);

	let path =
		PlaybackCache::frame_cache_path("/cache", "id", Rational::new(15, 1), Rational::new(1, 30));
	assert_eq!(
		path,
		std::path::Path::new("/cache")
			.join("id")
			.join("450")
			.to_string_lossy()
	);
	assert_eq!(
		PlaybackCache::frame_cache_path("/cache", "id", Rational::new(1, 10), Rational::new(1, 1)),
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
