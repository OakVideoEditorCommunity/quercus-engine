//! Safe wrapper over the engine's OTIO/FCPXML C ABI (`quercus_otio_*`).
//!
//! Two layers:
//!
//! - **Document level**: parse/serialize OTIO JSON ([`Document`]) and
//!   FCPXML ([`Timelines`]) — text in/out; file IO is the caller's
//!   (`std::fs`).
//! - **Structured level**: the object graph — [`Timeline`] →
//!   [`Track`] → [`Composable`] (clip/gap/transition) — as refcounted
//!   owned handles. Traversal clones out of the parent; edits write
//!   back through `set_track`/`set_child` (or `append_*`). This is the
//!   surface a functional plugin uses to inspect an imported
//!   interchange document or build a timeline from scratch and export
//!   it.

use std::ffi::c_void;

use crate::codec::string_out;

#[cfg_attr(all(windows, feature = "engine-cdylib"),
	windows,
	link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim")
)]
#[cfg_attr(all(not(windows), feature = "engine-cdylib"), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_otio_document_add_ref(doc: *const c_void);
	fn quercus_otio_document_release(doc: *const c_void);
	fn quercus_otio_timelines_add_ref(t: *const c_void);
	fn quercus_otio_timelines_release(t: *const c_void);
	fn quercus_otio_timeline_add_ref(t: *const c_void);
	fn quercus_otio_timeline_release(t: *const c_void);
	fn quercus_otio_track_add_ref(t: *const c_void);
	fn quercus_otio_track_release(t: *const c_void);
	fn quercus_otio_composable_add_ref(c: *const c_void);
	fn quercus_otio_composable_release(c: *const c_void);

	fn quercus_otio_document_from_json(json: *const u8, json_len: usize) -> *mut c_void;
	fn quercus_otio_document_to_json(doc: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_otio_document_kind(doc: *const c_void) -> i32;
	fn quercus_otio_document_timeline(doc: *const c_void) -> *mut c_void;
	fn quercus_otio_document_collection_timeline_count(doc: *const c_void) -> i32;
	fn quercus_otio_document_collection_timeline_at(doc: *const c_void, index: i32) -> *mut c_void;
	fn quercus_otio_document_from_timeline(timeline: *const c_void) -> *mut c_void;

	fn quercus_otio_timelines_new() -> *mut c_void;
	fn quercus_otio_timelines_from_fcpxml(xml: *const u8, xml_len: usize) -> *mut c_void;
	fn quercus_otio_timelines_to_fcpxml(t: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_otio_timelines_count(t: *const c_void) -> i32;
	fn quercus_otio_timelines_at(t: *const c_void, index: i32) -> *mut c_void;
	fn quercus_otio_timelines_set(t: *mut c_void, index: i32, timeline: *const c_void) -> bool;
	fn quercus_otio_timelines_add(t: *mut c_void, timeline: *const c_void) -> bool;

	fn quercus_otio_timeline_new(name: *const u8, name_len: usize) -> *mut c_void;
	fn quercus_otio_timeline_name(tl: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_otio_timeline_set_name(tl: *mut c_void, name: *const u8, name_len: usize) -> bool;
	fn quercus_otio_timeline_global_start_time(tl: *const c_void, out: *mut RationalTime) -> bool;
	fn quercus_otio_timeline_set_global_start_time(tl: *mut c_void, start: RationalTime) -> bool;
	fn quercus_otio_timeline_to_json(tl: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_otio_timeline_track_count(tl: *const c_void) -> i32;
	fn quercus_otio_timeline_track_at(tl: *const c_void, index: i32) -> *mut c_void;
	fn quercus_otio_timeline_set_track(tl: *mut c_void, index: i32, track: *const c_void) -> bool;
	fn quercus_otio_timeline_append_track(tl: *mut c_void, track: *const c_void) -> bool;
	fn quercus_otio_timeline_remove_track(tl: *mut c_void, index: i32) -> bool;

	fn quercus_otio_track_new(kind: *const u8, kind_len: usize) -> *mut c_void;
	fn quercus_otio_track_kind(t: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_otio_track_child_count(t: *const c_void) -> i32;
	fn quercus_otio_track_child_at(t: *const c_void, index: i32) -> *mut c_void;
	fn quercus_otio_track_set_child(t: *mut c_void, index: i32, child: *const c_void) -> bool;
	fn quercus_otio_track_append_child(t: *mut c_void, child: *const c_void) -> bool;
	fn quercus_otio_track_remove_child(t: *mut c_void, index: i32) -> bool;

	fn quercus_otio_composable_kind(c: *const c_void) -> i32;
	fn quercus_otio_composable_name(c: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_otio_composable_source_range(c: *const c_void, out: *mut TimeRange) -> bool;
	fn quercus_otio_clip_new(
		name: *const u8,
		name_len: usize,
		url: *const u8,
		url_len: usize,
		source_range: *const TimeRange,
	) -> *mut c_void;
	fn quercus_otio_gap_new(
		name: *const u8,
		name_len: usize,
		source_range: *const TimeRange,
	) -> *mut c_void;
	fn quercus_otio_transition_new(
		name: *const u8,
		name_len: usize,
		in_offset: RationalTime,
		out_offset: RationalTime,
	) -> *mut c_void;
	fn quercus_otio_clip_set_source_range(c: *mut c_void, range: TimeRange) -> bool;
	fn quercus_otio_clip_media_url(c: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_otio_clip_set_media_url(c: *mut c_void, url: *const u8, url_len: usize) -> bool;
	fn quercus_otio_clip_enabled(c: *const c_void) -> i32;
	fn quercus_otio_clip_set_enabled(c: *mut c_void, enabled: bool) -> bool;
	fn quercus_otio_transition_type(c: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_otio_transition_offset(c: *const c_void, is_in: bool, out: *mut RationalTime) -> bool;
	fn quercus_otio_transition_set_offset(c: *mut c_void, is_in: bool, offset: RationalTime) -> bool;
}

// ---------------------------------------------------------------------------
// Value types
// ---------------------------------------------------------------------------

/// A time value and its frame rate (OTIO `RationalTime`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RationalTime {
	/// The `value` field.
	pub value: f64,
	/// The `rate` field.
	pub rate: f64,
}

impl RationalTime {
	/// Builds a `RationalTime` from a raw `value` and the frame `rate` it is counted
	/// in.
	///
	/// `value` is measured in frames at `rate` frames per second, so the same instant
	/// has many equivalent representations; nothing is normalized here. Arguments
	/// follow the C++ `RationalTime(value, rate)` order that the engine expects. Use
	/// `to_seconds` for a rate-independent view.
	pub fn new(value: f64, rate: f64) -> Self {
		Self { value, rate }
	}

	/// Converts this time to seconds as `value / rate`.
	///
	/// `rate` is frames per second; the result is rate-independent wall-clock
	/// seconds. A `rate` of `0` is not rejected explicitly and therefore yields an
	/// infinity or `NaN`, so callers of untrusted documents should validate `rate`
	/// first.
	pub fn to_seconds(self) -> f64 {
		self.value / self.rate
	}
}

/// A duration and its start time (OTIO `TimeRange`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimeRange {
	/// The `start_time` field.
	pub start_time: RationalTime,
	/// The `duration` field.
	pub duration: RationalTime,
}

impl TimeRange {
	/// Builds a `TimeRange` spanning `duration` from `start_time`.
	///
	/// Arguments follow the C++ `TimeRange(start_time, duration)` order, which is the
	/// reverse of the OTIO JSON field order; both times are stored verbatim and are
	/// expected to share the same `rate` — nothing is converted or validated. Used by
	/// `Composable::clip`/`Composable::gap` to describe a used source portion.
	pub fn new(start_time: RationalTime, duration: RationalTime) -> Self {
		Self {
			start_time,
			duration,
		}
	}
}

/// The root kind of a [`Document`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentKind {
	/// An OpenTimelineIO Timeline.
	Timeline,
	/// An OpenTimelineIO Collection.
	Collection,
	/// An unrecognized root schema (kept whole for round-tripping).
	Raw,
}

/// The kind of a [`Composable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposableKind {
	/// A Clip composable.
	Clip,
	/// A Gap composable.
	Gap,
	/// A Transition composable.
	Transition,
	/// Nested track/stack or an unrecognized schema (kept whole).
	Other,
}

/// OTIO track kinds.
pub mod track_kind {
	/// The `VIDEO` constant.
	pub const VIDEO: &str = "Video";
	/// The `AUDIO` constant.
	pub const AUDIO: &str = "Audio";
}

// ---------------------------------------------------------------------------
// Handle boilerplate
// ---------------------------------------------------------------------------

macro_rules! handle {
	($name:ident, $add_ref:ident, $release:ident) => {
		impl Clone for $name {
			fn clone(&self) -> Self {
				unsafe { $add_ref(self.ptr) };
				Self { ptr: self.ptr }
			}
		}
		impl Drop for $name {
			fn drop(&mut self) {
				unsafe { $release(self.ptr) };
			}
		}
		// The engine side guards every handle with a mutex; the refcount
		// is atomic.
		unsafe impl Send for $name {}
		unsafe impl Sync for $name {}
	};
}

// ---------------------------------------------------------------------------
// Document (OTIO JSON root)
// ---------------------------------------------------------------------------

/// A parsed OTIO document (the root serializable object).
pub struct Document {
	ptr: *mut c_void,
}

handle!(
	Document,
	quercus_otio_document_add_ref,
	quercus_otio_document_release
);

impl Document {
	/// Parses an OTIO JSON document from the full document text `json`.
	///
	/// `json` is borrowed only for the call; the returned `Document` owns a
	/// refcounted engine handle, so it can outlive the input string and be cloned
	/// cheaply. Returns `None` when the engine rejects the input — malformed JSON or
	/// an unsupported root schema — which is the only error channel; the reason is
	/// not reported. Counterpart of `Document::to_json`.
	pub fn from_json(json: &str) -> Option<Document> {
		let ptr = unsafe { quercus_otio_document_from_json(json.as_ptr(), json.len()) };
		(!ptr.is_null()).then_some(Document { ptr })
	}

	/// Serializes to OTIO JSON.
	pub fn to_json(&self) -> String {
		string_out(|buf, len| unsafe { quercus_otio_document_to_json(self.ptr, buf, len) })
			.unwrap_or_default()
	}

	/// The root schema of the parsed document.
	///
	/// Returns `DocumentKind::Timeline` or `DocumentKind::Collection` for those two
	/// OTIO roots, and `DocumentKind::Raw` for any other root schema, which is kept
	/// whole so the document can round-trip. Returns `None` only for an unexpected
	/// value from the engine (never for a successfully parsed document). Follow up
	/// with `Document::timeline` or `Document::collection_timelines` to reach the
	/// object graph.
	pub fn kind(&self) -> Option<DocumentKind> {
		match unsafe { quercus_otio_document_kind(self.ptr) } {
			0 => Some(DocumentKind::Timeline),
			1 => Some(DocumentKind::Collection),
			2 => Some(DocumentKind::Raw),
			_ => None,
		}
	}

	/// The root timeline (None when the root is not a timeline).
	pub fn timeline(&self) -> Option<Timeline> {
		let ptr = unsafe { quercus_otio_document_timeline(self.ptr) };
		(!ptr.is_null()).then_some(Timeline { ptr })
	}

	/// The timeline children of a collection root.
	pub fn collection_timelines(&self) -> Vec<Timeline> {
		let n = unsafe { quercus_otio_document_collection_timeline_count(self.ptr) }.max(0);
		(0..n)
			.filter_map(|i| {
				let ptr = unsafe { quercus_otio_document_collection_timeline_at(self.ptr, i) };
				(!ptr.is_null()).then_some(Timeline { ptr })
			})
			.collect()
	}

	/// Wraps a live `timeline` in a `Document` whose root is that timeline.
	///
	/// The timeline is shared, not copied: the document takes its own reference, so
	/// `Document::timeline` returns a handle to the same object and later edits to
	/// `timeline` are visible through the document. This is the usual route from a
	/// timeline built with `Timeline` to `Document::to_json`.
	///
	/// # Panics
	///
	/// Panics if the engine returns a null handle, which the wrapper treats as
	/// impossible for a live timeline.
	pub fn from_timeline(timeline: &Timeline) -> Document {
		let ptr = unsafe { quercus_otio_document_from_timeline(timeline.ptr) };
		assert!(!ptr.is_null(), "the engine wraps any live timeline");
		Document { ptr }
	}
}

// ---------------------------------------------------------------------------
// Timelines (FCPXML document unit / export builder)
// ---------------------------------------------------------------------------

/// A list of timelines (FCPXML parses to one timeline per
/// `<sequence>`; the same list is the FCPXML export input).
pub struct Timelines {
	ptr: *mut c_void,
}

handle!(
	Timelines,
	quercus_otio_timelines_add_ref,
	quercus_otio_timelines_release
);

impl Timelines {
	/// A new, empty list.
	pub fn new() -> Timelines {
		Timelines {
			ptr: unsafe { quercus_otio_timelines_new() },
		}
	}

	/// Parses an FCPXML document from the full document text `xml`.
	///
	/// FCPXML may hold several `<sequence>` elements; the engine turns each into one
	/// `Timeline`, so a successful parse can produce an empty or multi-entry list even
	/// though FCPXML is usually one sequence. Returns `None` on a parse error or when
	/// the input cannot be represented. Counterpart of `Timelines::to_fcpxml`.
	pub fn from_fcpxml(xml: &str) -> Option<Timelines> {
		let ptr = unsafe { quercus_otio_timelines_from_fcpxml(xml.as_ptr(), xml.len()) };
		(!ptr.is_null()).then_some(Timelines { ptr })
	}

	/// Serializes every timeline in the list to one FCPXML document.
	///
	/// This is the export counterpart of `Timelines::from_fcpxml`: it emits one
	/// `<sequence>` per timeline, in list order. Encoding failures are swallowed and
	/// surface as an empty string, so callers that must not silently lose data should
	/// treat `""` as an error.
	pub fn to_fcpxml(&self) -> String {
		string_out(|buf, len| unsafe { quercus_otio_timelines_to_fcpxml(self.ptr, buf, len) })
			.unwrap_or_default()
	}

	/// Number of timelines.
	pub fn len(&self) -> usize {
		unsafe { quercus_otio_timelines_count(self.ptr) }.max(0) as usize
	}

	/// Whether the list is empty.
	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}

	/// The timeline at `index` (a clone; edit and write back with
	/// [`Timelines::set`]).
	pub fn at(&self, index: usize) -> Option<Timeline> {
		let ptr = unsafe { quercus_otio_timelines_at(self.ptr, index as i32) };
		(!ptr.is_null()).then_some(Timeline { ptr })
	}

	/// Replaces the timeline at `index` (write-back after editing).
	pub fn set(&self, index: usize, timeline: &Timeline) -> bool {
		unsafe { quercus_otio_timelines_set(self.ptr, index as i32, timeline.ptr) }
	}

	/// Appends `timeline` to the list, adding one more `<sequence>` on FCPXML
	/// export.
	///
	/// The timeline is refcounted and shared, not copied; later edits to `timeline`
	/// are reflected in the list. Returns `false` when the engine rejects the append,
	/// leaving the list unchanged. Pairs with `Timelines::at`/`Timelines::set`.
	pub fn push(&self, timeline: &Timeline) -> bool {
		unsafe { quercus_otio_timelines_add(self.ptr, timeline.ptr) }
	}
}

impl Default for Timelines {
	fn default() -> Self {
		Self::new()
	}
}

// ---------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------

/// A timeline (name, global start time, tracks).
pub struct Timeline {
	ptr: *mut c_void,
}

handle!(
	Timeline,
	quercus_otio_timeline_add_ref,
	quercus_otio_timeline_release
);

impl Timeline {
	/// Creates a timeline named `name` with no tracks.
	///
	/// `name` is copied into the timeline and can be changed later with
	/// `Timeline::set_name`. Build the track stack with `Timeline::append_track` (or
	/// wrap an existing timeline with `Document::from_timeline`) and then serialize
	/// with `Document::to_json`/`Timelines::to_fcpxml`.
	pub fn new(name: &str) -> Timeline {
		Timeline {
			ptr: unsafe { quercus_otio_timeline_new(name.as_ptr(), name.len()) },
		}
	}

	/// The timeline's name as UTF-8 text.
	///
	/// Returns an empty string when the engine cannot produce the name (for example
	/// a failed UTF-8 conversion); there is no separate error channel. Pairs with
	/// `Timeline::set_name`.
	pub fn name(&self) -> String {
		string_out(|buf, len| unsafe { quercus_otio_timeline_name(self.ptr, buf, len) })
			.unwrap_or_default()
	}

	/// Renames the timeline, replacing the previous name with `name`.
	///
	/// `name` is copied, so the caller keeps ownership. Returns `false` when the
	/// engine rejects the new name, in which case the old name is kept. Getter:
	/// `Timeline::name`.
	pub fn set_name(&self, name: &str) -> bool {
		unsafe { quercus_otio_timeline_set_name(self.ptr, name.as_ptr(), name.len()) }
	}

	/// The timeline's global start time, if one is set.
	///
	/// Returns `None` for a timeline with no global start time (the common case for
	/// a freshly created or imported timeline); otherwise the `RationalTime` in the
	/// timeline's own rate. Pairs with `Timeline::set_global_start_time`.
	pub fn global_start_time(&self) -> Option<RationalTime> {
		let mut out = RationalTime::default();
		unsafe { quercus_otio_timeline_global_start_time(self.ptr, &mut out) }.then_some(out)
	}

	/// Sets the timeline's global start time (OTIO `global_start_time`), the time
	/// value of the timeline's first frame.
	///
	/// `start` is copied and its `rate` should match the timeline's, since no
	/// resampling happens here. Returns `false` when the engine rejects the value.
	/// Getter: `Timeline::global_start_time`.
	pub fn set_global_start_time(&self, start: RationalTime) -> bool {
		unsafe { quercus_otio_timeline_set_global_start_time(self.ptr, start) }
	}

	/// Serializes the timeline alone to OTIO JSON.
	pub fn to_json(&self) -> String {
		string_out(|buf, len| unsafe { quercus_otio_timeline_to_json(self.ptr, buf, len) })
			.unwrap_or_default()
	}

	/// Number of tracks.
	pub fn track_count(&self) -> usize {
		unsafe { quercus_otio_timeline_track_count(self.ptr) }.max(0) as usize
	}

	/// The track at `index` (a clone; edit and write back with
	/// [`Timeline::set_track`]).
	pub fn track_at(&self, index: usize) -> Option<Track> {
		let ptr = unsafe { quercus_otio_timeline_track_at(self.ptr, index as i32) };
		(!ptr.is_null()).then_some(Track { ptr })
	}

	/// All tracks of the timeline in stack order, as shared handle clones.
	///
	/// Collects every index through `Timeline::track_at`, so the length matches
	/// `Timeline::track_count`. Mutating a returned track only affects the clone
	/// until it is published back with `Timeline::set_track`.
	pub fn tracks(&self) -> Vec<Track> {
		(0..self.track_count())
			.filter_map(|i| self.track_at(i))
			.collect()
	}

	/// Replaces the track at `index` with `track`, publishing edits made to a clone.
	///
	/// Traversal returns clones (`Timeline::track_at`/`Timeline::tracks`), so this is
	/// the call that writes an edited track back into the timeline. `index` is
	/// zero-based and must be below `Timeline::track_count`; an out-of-range index
	/// returns `false` and leaves the timeline unchanged.
	pub fn set_track(&self, index: usize, track: &Track) -> bool {
		unsafe { quercus_otio_timeline_set_track(self.ptr, index as i32, track.ptr) }
	}

	/// Appends `track` after the last track of the stack.
	///
	/// The track is refcounted and shared, not copied. Returns `false` when the engine
	/// rejects the append, leaving the stack unchanged. Use `Timeline::set_track` or
	/// `Timeline::remove_track` to change existing entries.
	pub fn append_track(&self, track: &Track) -> bool {
		unsafe { quercus_otio_timeline_append_track(self.ptr, track.ptr) }
	}

	/// Removes the track at `index`, shifting later tracks down by one.
	///
	/// `index` is zero-based and must be below `Timeline::track_count`; an
	/// out-of-range index returns `false` and leaves the stack unchanged. The engine
	/// drops its own reference to the removed track, so existing clones stay valid.
	pub fn remove_track(&self, index: usize) -> bool {
		unsafe { quercus_otio_timeline_remove_track(self.ptr, index as i32) }
	}
}

// ---------------------------------------------------------------------------
// Track
// ---------------------------------------------------------------------------

/// A track (kind + ordered composables).
pub struct Track {
	ptr: *mut c_void,
}

handle!(Track, quercus_otio_track_add_ref, quercus_otio_track_release);

impl Track {
	/// A new track of the given kind ([`track_kind::VIDEO`],
	/// [`track_kind::AUDIO`], ...).
	pub fn new(kind: &str) -> Track {
		Track {
			ptr: unsafe { quercus_otio_track_new(kind.as_ptr(), kind.len()) },
		}
	}

	/// The track kind ("Video", "Audio", ...).
	pub fn kind(&self) -> String {
		string_out(|buf, len| unsafe { quercus_otio_track_kind(self.ptr, buf, len) })
			.unwrap_or_default()
	}

	/// Number of children (clips/gaps/transitions).
	pub fn child_count(&self) -> usize {
		unsafe { quercus_otio_track_child_count(self.ptr) }.max(0) as usize
	}

	/// The child at `index` (a clone; edit and write back with
	/// [`Track::set_child`]).
	pub fn child_at(&self, index: usize) -> Option<Composable> {
		let ptr = unsafe { quercus_otio_track_child_at(self.ptr, index as i32) };
		(!ptr.is_null()).then_some(Composable { ptr })
	}

	/// All children (clips, gaps, transitions) in track order, as shared handle
	/// clones.
	///
	/// Collects every index through `Track::child_at`, so the length matches
	/// `Track::child_count`. Mutating a returned child only affects the clone until it
	/// is published with `Track::set_child`.
	pub fn children(&self) -> Vec<Composable> {
		(0..self.child_count())
			.filter_map(|i| self.child_at(i))
			.collect()
	}

	/// Replaces the child at `index` with `child`, publishing edits made to a clone.
	///
	/// Composable handles are clones, so this is the write-back step after mutating a
	/// child obtained from `Track::child_at`/`Track::children`. `index` is zero-based
	/// and must be below `Track::child_count`; an out-of-range index returns `false`
	/// and leaves the track unchanged.
	pub fn set_child(&self, index: usize, child: &Composable) -> bool {
		unsafe { quercus_otio_track_set_child(self.ptr, index as i32, child.ptr) }
	}

	/// Appends `child` (a clip, gap, or transition) after the last composable.
	///
	/// The composable is refcounted and shared, not copied. Returns `false` when the
	/// engine rejects the append, leaving the track unchanged. Appending is how a
	/// track is built from scratch; order is preserved for playback and export.
	pub fn append_child(&self, child: &Composable) -> bool {
		unsafe { quercus_otio_track_append_child(self.ptr, child.ptr) }
	}

	/// Removes the child at `index`, shifting later children down by one.
	///
	/// `index` is zero-based and must be below `Track::child_count`; an out-of-range
	/// index returns `false` and leaves the track unchanged. Removing a clip does not
	/// retime its neighbours — insert a gap to preserve their positions.
	pub fn remove_child(&self, index: usize) -> bool {
		unsafe { quercus_otio_track_remove_child(self.ptr, index as i32) }
	}
}

// ---------------------------------------------------------------------------
// Composable (clip / gap / transition)
// ---------------------------------------------------------------------------

/// A track/stack child: a clip, gap, or transition.
pub struct Composable {
	ptr: *mut c_void,
}

handle!(
	Composable,
	quercus_otio_composable_add_ref,
	quercus_otio_composable_release
);

impl Composable {
	/// Builds a clip composable that references external media.
	///
	/// `name` is the display name. `url` is the optional external media reference:
	/// `Some(url)` stores a reference pointing at `url` (typically a `file://` URL),
	/// while `None` stores a missing reference and `Composable::clip_media_url` then
	/// returns `None`. `source_range` is the optional portion of the source media the
	/// clip uses, in the media's own rate.
	///
	/// # Panics
	///
	/// Panics if the engine returns a null handle, which the wrapper treats as
	/// impossible for valid input.
	pub fn clip(name: &str, url: Option<&str>, source_range: Option<TimeRange>) -> Composable {
		let (u, ul) = url.map_or((std::ptr::null(), 0), |u| (u.as_ptr(), u.len()));
		let range = source_range
			.as_ref()
			.map_or(std::ptr::null(), |r| r as *const TimeRange);
		let ptr = unsafe { quercus_otio_clip_new(name.as_ptr(), name.len(), u, ul, range) };
		assert!(!ptr.is_null(), "valid input always builds a clip");
		Composable { ptr }
	}

	/// Builds a gap composable that occupies time without any media.
	///
	/// `name` is the display name. `source_range` gives the gap's extent in its own
	/// rate; `None` stores an empty (zero-duration) range, which is the engine's
	/// default. Gaps keep later clips at their intended timeline positions.
	///
	/// # Panics
	///
	/// Panics if the engine returns a null handle, which the wrapper treats as
	/// impossible for valid input.
	pub fn gap(name: &str, source_range: Option<TimeRange>) -> Composable {
		let range = source_range
			.as_ref()
			.map_or(std::ptr::null(), |r| r as *const TimeRange);
		let ptr = unsafe { quercus_otio_gap_new(name.as_ptr(), name.len(), range) };
		assert!(!ptr.is_null(), "valid input always builds a gap");
		Composable { ptr }
	}

	/// Builds a transition composable that blends two adjacent clips.
	///
	/// `name` is also the transition type (e.g. `"SMPTE_Dissolve"`), readable via
	/// `Composable::transition_type`. `in_offset` and `out_offset` are the extents
	/// the transition reaches into the incoming and outgoing clip, in the timeline
	/// rate.
	///
	/// # Panics
	///
	/// Panics if the engine returns a null handle, which the wrapper treats as
	/// impossible for valid input.
	pub fn transition(name: &str, in_offset: RationalTime, out_offset: RationalTime) -> Composable {
		let ptr =
			unsafe { quercus_otio_transition_new(name.as_ptr(), name.len(), in_offset, out_offset) };
		assert!(!ptr.is_null(), "valid input always builds a transition");
		Composable { ptr }
	}

	/// The concrete composable schema behind this handle.
	///
	/// Returns `ComposableKind::Clip`, `ComposableKind::Gap` or
	/// `ComposableKind::Transition` for those schemas, and `ComposableKind::Other` for
	/// a nested track/stack child or any unrecognized schema (kept whole). Returns
	/// `None` only for an unexpected value from the engine. Branch on this before
	/// calling the clip- or transition-specific accessors, which answer `false`/`None`
	/// for the wrong kind.
	pub fn kind(&self) -> Option<ComposableKind> {
		match unsafe { quercus_otio_composable_kind(self.ptr) } {
			0 => Some(ComposableKind::Clip),
			1 => Some(ComposableKind::Gap),
			2 => Some(ComposableKind::Transition),
			3 => Some(ComposableKind::Other),
			_ => None,
		}
	}

	/// The name ("" for unknown blocks).
	pub fn name(&self) -> String {
		string_out(|buf, len| unsafe { quercus_otio_composable_name(self.ptr, buf, len) })
			.unwrap_or_default()
	}

	/// The source range of clips and gaps (None for other kinds or an
	/// unset range).
	pub fn source_range(&self) -> Option<TimeRange> {
		let mut out = TimeRange::default();
		unsafe { quercus_otio_composable_source_range(self.ptr, &mut out) }.then_some(out)
	}

	/// Sets the source range of a clip — the portion of its source media it uses.
	///
	/// `range` is in the media's own rate and replaces the previous value. Returns
	/// `false` when the composable is not a clip; the range is then unchanged. Getter:
	/// `Composable::source_range`, which also reports gaps.
	pub fn set_clip_source_range(&self, range: TimeRange) -> bool {
		unsafe { quercus_otio_clip_set_source_range(self.ptr, range) }
	}

	/// The clip's media URL (None for a non-clip or a non-external —
	/// e.g. missing — media reference).
	pub fn clip_media_url(&self) -> Option<String> {
		string_out(|buf, len| unsafe { quercus_otio_clip_media_url(self.ptr, buf, len) })
	}

	/// Points the clip's media reference at `url`. False for a
	/// non-clip.
	pub fn set_clip_media_url(&self, url: &str) -> bool {
		unsafe { quercus_otio_clip_set_media_url(self.ptr, url.as_ptr(), url.len()) }
	}

	/// Whether the clip is enabled (None for a non-clip).
	pub fn clip_enabled(&self) -> Option<bool> {
		match unsafe { quercus_otio_clip_enabled(self.ptr) } {
			0 => Some(false),
			1 => Some(true),
			_ => None,
		}
	}

	/// Enables or disables a clip.
	///
	/// `enabled == false` marks the clip as not contributing media while it keeps its
	/// place and timing in the track. Returns `false` when the composable is not a
	/// clip, in which case the flag is unchanged. Getter:
	/// `Composable::clip_enabled`.
	pub fn set_clip_enabled(&self, enabled: bool) -> bool {
		unsafe { quercus_otio_clip_set_enabled(self.ptr, enabled) }
	}

	/// The transition type ("SMPTE_Dissolve", ...; None for a
	/// non-transition).
	pub fn transition_type(&self) -> Option<String> {
		string_out(|buf, len| unsafe { quercus_otio_transition_type(self.ptr, buf, len) })
	}

	/// One side's offset of a transition, in the timeline rate.
	///
	/// `is_in` selects the in-side (`true`) or out-side (`false`) offset. Returns
	/// `None` when the composable is not a transition, and also when the requested
	/// offset is unset. Setter: `Composable::set_transition_offset`.
	pub fn transition_offset(&self, is_in: bool) -> Option<RationalTime> {
		let mut out = RationalTime::default();
		unsafe { quercus_otio_transition_offset(self.ptr, is_in, &mut out) }.then_some(out)
	}

	/// Sets one side's offset of a transition.
	///
	/// `is_in` selects the in-side (`true`) or out-side (`false`) offset; `offset` is
	/// in the timeline rate and replaces the previous value. Returns `false` when the
	/// composable is not a transition, in which case nothing changes. Getter:
	/// `Composable::transition_offset`.
	pub fn set_transition_offset(&self, is_in: bool, offset: RationalTime) -> bool {
		unsafe { quercus_otio_transition_set_offset(self.ptr, is_in, offset) }
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The golden OTIO fixture shipped with quercus-otio.
	fn golden_timeline_json() -> String {
		let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
			.join("../quercus-otio/tests/data/golden_timeline.json");
		std::fs::read_to_string(path).expect("golden fixture")
	}

	#[test]
	fn parse_golden_and_traverse() {
		let doc = Document::from_json(&golden_timeline_json()).expect("parse");
		assert_eq!(doc.kind(), Some(DocumentKind::Timeline));
		let timeline = doc.timeline().expect("timeline root");
		assert_eq!(timeline.name(), "My Sequence");
		assert!(timeline.track_count() > 0);
		let track = timeline.track_at(0).expect("track 0");
		assert_eq!(track.kind(), "Video");
		for child in track.children() {
			match child.kind() {
				Some(ComposableKind::Clip) => {
					let _ = child.source_range();
					let _ = child.clip_media_url();
					let _ = child.clip_enabled();
				}
				Some(ComposableKind::Gap) => {
					let _ = child.source_range();
				}
				_ => {}
			}
		}
		// The document survives a JSON round-trip byte-for-byte.
		let json = doc.to_json();
		let reparsed = Document::from_json(&json).expect("reparse");
		assert_eq!(reparsed.to_json(), json);
	}

	#[test]
	fn build_edit_and_fcpxml_roundtrip() {
		let timeline = Timeline::new("Rough Cut");
		assert!(timeline.set_global_start_time(RationalTime::new(0.0, 24.0)));

		let track = Track::new(track_kind::VIDEO);
		let range = TimeRange::new(RationalTime::new(10.0, 24.0), RationalTime::new(48.0, 24.0));
		let clip = Composable::clip("clip1", Some("file:///tmp/a.mov"), Some(range));
		assert_eq!(clip.kind(), Some(ComposableKind::Clip));
		track.append_child(&clip);
		track.append_child(&Composable::gap("gap", None));
		timeline.append_track(&track);

		// Clone-out traversal + write-back edit.
		let t0 = timeline.track_at(0).expect("track 0");
		assert_eq!(t0.child_count(), 2);
		let mut c0 = t0.child_at(0).expect("child 0");
		assert_eq!(c0.name(), "clip1");
		assert_eq!(c0.clip_media_url().as_deref(), Some("file:///tmp/a.mov"));
		assert_eq!(c0.source_range().unwrap().duration.value, 48.0);
		assert!(c0.set_clip_media_url("file:///tmp/b.mov"));
		assert!(c0.set_clip_enabled(false));
		assert!(t0.set_child(0, &c0));
		assert!(timeline.set_track(0, &t0));
		let t0 = timeline.track_at(0).expect("track 0");
		c0 = t0.child_at(0).expect("child 0");
		assert_eq!(c0.clip_media_url().as_deref(), Some("file:///tmp/b.mov"));
		assert_eq!(c0.clip_enabled(), Some(false));

		// JSON via the document layer.
		let doc = Document::from_timeline(&timeline);
		let json = doc.to_json();
		let reparsed = Document::from_json(&json).expect("reparse");
		assert_eq!(reparsed.timeline().unwrap().name(), "Rough Cut");

		// FCPXML round-trip.
		let list = Timelines::new();
		assert!(list.push(&timeline));
		let xml = list.to_fcpxml();
		let back = Timelines::from_fcpxml(&xml).expect("fcpxml parse");
		assert_eq!(back.len(), 1);
		assert_eq!(back.at(0).unwrap().name(), "Rough Cut");
		assert!(Timelines::from_fcpxml("not xml").is_none());
	}

	#[test]
	fn invalid_input_fails_closed() {
		assert!(Document::from_json("{").is_none());
		assert!(Timelines::from_fcpxml("").is_none());
		let gap = Composable::gap("g", None);
		assert_eq!(gap.kind(), Some(ComposableKind::Gap));
		assert!(gap.clip_media_url().is_none());
		assert!(!gap.set_clip_enabled(true));
		let t = Composable::transition(
			"t",
			RationalTime::new(1.0, 24.0),
			RationalTime::new(1.0, 24.0),
		);
		assert_eq!(
			t.transition_offset(true),
			Some(RationalTime::new(1.0, 24.0))
		);
		assert!(t.set_transition_offset(false, RationalTime::new(2.0, 24.0)));
		assert_eq!(
			t.transition_offset(false),
			Some(RationalTime::new(2.0, 24.0))
		);
	}
}
