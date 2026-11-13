//! C ABI for quercus-otio (prefix `quercus_otio_`): the OpenTimelineIO JSON and
//! FCPXML interchange binding.
//!
//! Two layers:
//!
//! - **Document level** (string in/out): parse/serialize OTIO JSON
//!   ([`quercus_otio_document_from_json`]/[`quercus_otio_document_to_json`]) and
//!   FCPXML ([`quercus_otio_timelines_from_fcpxml`]/[`quercus_otio_timelines_to_fcpxml`]).
//!   File IO is the consumer's business (the SDK wraps it).
//! - **Structured level**: the object graph — timeline → tracks →
//!   composables (clip/gap/transition) — as refcounted owned handles.
//!   Traversal clones out of the parent; edits write back through
//!   `set_track`/`set_child` (or `append_child`). This is the surface a
//!   functional plugin uses to inspect an imported interchange document
//!   or build a timeline from scratch (e.g. an AI rough cut) and export
//!   it.
//!
//! Conventions (shared with the rest of the engine ABI): string
//! arguments are `(ptr, len)` borrows; string outputs are
//! snprintf-style (`copy_str_out`); handles are refcounted with a typed
//! `add_ref`/`release` pair; all entry points are null-safe and never
//! panic across the boundary.

use std::sync::Mutex;

use quercus_otio::model::{
	Clip, Composable, ExternalReference, Gap, MediaReference, RationalTime, Serializable,
	TimeRange, Track, Transition,
};
use quercus_otio::model::Timeline;

use crate::codec::{copy_str_out, str_arg};
use crate::handle::NativeMirror;

// ---------------------------------------------------------------------------
// POD mirrors
// ---------------------------------------------------------------------------

/// `repr(C)` mirror of quercus-otio's `RationalTime` (a time value and its
/// frame rate).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QuercusOtioRationalTime {
	pub value: f64,
	pub rate: f64,
}

impl NativeMirror for QuercusOtioRationalTime {
	type Native = RationalTime;

	fn from_native(t: &RationalTime) -> Self {
		Self {
			value: t.value(),
			rate: t.rate(),
		}
	}

	fn to_native(&self) -> RationalTime {
		RationalTime::new(self.value, self.rate)
	}
}

/// `repr(C)` mirror of quercus-otio's `TimeRange` (a start time and a
/// duration).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QuercusOtioTimeRange {
	pub start_time: QuercusOtioRationalTime,
	pub duration: QuercusOtioRationalTime,
}

impl NativeMirror for QuercusOtioTimeRange {
	type Native = TimeRange;

	fn from_native(r: &TimeRange) -> Self {
		Self {
			start_time: QuercusOtioRationalTime::from_native(&r.start_time()),
			duration: QuercusOtioRationalTime::from_native(&r.duration()),
		}
	}

	fn to_native(&self) -> TimeRange {
		TimeRange::new(self.start_time.to_native(), self.duration.to_native())
	}
}

// ---------------------------------------------------------------------------
// Handles
// ---------------------------------------------------------------------------

/// A parsed OTIO document (the root `Serializable`).
pub struct QuercusOtioDocument {
	inner: Mutex<Serializable>,
}

/// A list of timelines (the FCPXML document unit: one timeline per
/// sequence; also the builder for FCPXML export).
pub struct QuercusOtioTimelines {
	inner: Mutex<Vec<Timeline>>,
}

/// A timeline (name, global start time, tracks).
pub struct QuercusOtioTimeline {
	inner: Mutex<Timeline>,
}

/// A track (kind + ordered composables).
pub struct QuercusOtioTrack {
	inner: Mutex<Track>,
}

/// A track/stack child: a clip, gap, or transition.
pub struct QuercusOtioComposable {
	inner: Mutex<Composable>,
}

crate::export_handle!(
	QuercusOtioDocument,
	quercus_otio_document_add_ref,
	quercus_otio_document_release
);
crate::export_handle!(
	QuercusOtioTimelines,
	quercus_otio_timelines_add_ref,
	quercus_otio_timelines_release
);
crate::export_handle!(
	QuercusOtioTimeline,
	quercus_otio_timeline_add_ref,
	quercus_otio_timeline_release
);
crate::export_handle!(QuercusOtioTrack, quercus_otio_track_add_ref, quercus_otio_track_release);
crate::export_handle!(
	QuercusOtioComposable,
	quercus_otio_composable_add_ref,
	quercus_otio_composable_release
);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `quercus_otio_document_kind` result: the root is a Timeline.
pub const QUERCUS_OTIO_DOC_TIMELINE: i32 = 0;
/// `quercus_otio_document_kind` result: the root is a SerializableCollection.
pub const QUERCUS_OTIO_DOC_COLLECTION: i32 = 1;
/// `quercus_otio_document_kind` result: an unrecognized root schema (kept
/// whole for round-tripping).
pub const QUERCUS_OTIO_DOC_RAW: i32 = 2;

/// `quercus_otio_composable_kind` result: a clip.
pub const QUERCUS_OTIO_COMPOSABLE_CLIP: i32 = 0;
/// `quercus_otio_composable_kind` result: a gap.
pub const QUERCUS_OTIO_COMPOSABLE_GAP: i32 = 1;
/// `quercus_otio_composable_kind` result: a transition.
pub const QUERCUS_OTIO_COMPOSABLE_TRANSITION: i32 = 2;
/// `quercus_otio_composable_kind` result: anything else (nested track/stack,
/// or an unrecognized schema kept whole).
pub const QUERCUS_OTIO_COMPOSABLE_OTHER: i32 = 3;

// ---------------------------------------------------------------------------
// Document (OTIO JSON root)
// ---------------------------------------------------------------------------

/// Parses an OTIO JSON document into an opaque document handle.
///
/// `json` is a borrowed `(ptr, len)` byte slice holding a UTF-8 OTIO JSON
/// string. It is only read during the call and never stored. The returned
/// handle starts at ref count 1 and must be released with
/// `quercus_otio_document_release`.
///
/// # Returns
///
/// A new [`QuercusOtioDocument`], or null when `json` is null, is not valid
/// UTF-8, or the text fails to parse as OTIO JSON.
///
/// # Safety
///
/// `json` must be null or point to `json_len` readable bytes for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_document_from_json(
	json: *const u8,
	json_len: usize,
) -> *mut QuercusOtioDocument {
	let Some(json) = (unsafe { str_arg(json, json_len) }) else {
		return std::ptr::null_mut();
	};
	match quercus_otio::from_json_string(json) {
		Ok(root) => crate::handle::into_ffi(QuercusOtioDocument {
			inner: Mutex::new(root),
		}) as *mut QuercusOtioDocument,
		Err(_) => std::ptr::null_mut(),
	}
}

/// Serializes the whole document to OTIO JSON, snprintf-style.
///
/// Writes at most `buf_len - 1` bytes plus a trailing NUL into `buf`; pass
/// `buf = null, buf_len = 0` to query the required size. The root is
/// emitted as-is, so raw (unrecognized) documents round-trip unchanged.
///
/// # Returns
///
/// The byte length of the JSON (excluding the NUL), which may exceed the
/// truncated write, or -1 when `doc` is null or serialization fails.
///
/// # Safety
///
/// `doc` must be a live document handle (or null); `buf` must be null or
/// point to `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_document_to_json(
	doc: *const QuercusOtioDocument,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	if doc.is_null() {
		return -1;
	}
	let root = lock(&unsafe { &*doc }.inner);
	match root.to_json_string() {
		Ok(json) => {
			drop(root);
			copy_str_out(&json, buf, buf_len)
		}
		Err(_) => -1,
	}
}

/// Classifies the document's root node.
///
/// # Returns
///
/// [`QUERCUS_OTIO_DOC_TIMELINE`], [`QUERCUS_OTIO_DOC_COLLECTION`] or
/// [`QUERCUS_OTIO_DOC_RAW`], or -1 when `doc` is null.
///
/// # Safety
///
/// `doc` must be a live document handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_document_kind(doc: *const QuercusOtioDocument) -> i32 {
	if doc.is_null() {
		return -1;
	}
	match &*lock(&unsafe { &*doc }.inner) {
		Serializable::Timeline(_) => QUERCUS_OTIO_DOC_TIMELINE,
		Serializable::SerializableCollection(_) => QUERCUS_OTIO_DOC_COLLECTION,
		Serializable::Raw(_) => QUERCUS_OTIO_DOC_RAW,
	}
}

/// Clones the document's root timeline out of the document.
///
/// The clone is detached from the document: editing it does not write back.
/// Use it for inspection, or pass it to
/// `quercus_otio_document_from_timeline` to rebuild a document root.
///
/// # Returns
///
/// A new timeline handle at ref count 1, or null when `doc` is null or the
/// root is not a timeline (collection or raw).
///
/// # Safety
///
/// `doc` must be a live document handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_document_timeline(
	doc: *const QuercusOtioDocument,
) -> *mut QuercusOtioTimeline {
	if doc.is_null() {
		return std::ptr::null_mut();
	}
	let root = lock(&unsafe { &*doc }.inner);
	match root.as_timeline().cloned() {
		Some(tl) => {
			drop(root);
			crate::handle::into_ffi(QuercusOtioTimeline {
				inner: Mutex::new(tl),
			}) as *mut QuercusOtioTimeline
		}
		None => std::ptr::null_mut(),
	}
}

/// Counts the timeline children of a collection root.
///
/// Only children that are timelines are counted, which makes the result the
/// index space of `quercus_otio_document_collection_timeline_at`.
///
/// # Returns
///
/// The number of timeline children, 0 for a non-collection root, or -1 when
/// `doc` is null.
///
/// # Safety
///
/// `doc` must be a live document handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_document_collection_timeline_count(
	doc: *const QuercusOtioDocument,
) -> i32 {
	if doc.is_null() {
		return -1;
	}
	let root = lock(&unsafe { &*doc }.inner);
	match root.as_collection() {
		Some(c) => c
			.children()
			.iter()
			.filter(|s| matches!(***s, Serializable::Timeline(_)))
			.count() as i32,
		None => 0,
	}
}

/// Clones the `index`-th timeline child out of a collection root.
///
/// `index` is 0-based and counts timeline children only; other children of
/// the collection are skipped. The returned handle is a detached copy.
///
/// # Returns
///
/// A new timeline handle at ref count 1, or null when `doc` is null, `index`
/// is negative, the root is not a collection, or `index` is past the last
/// timeline child.
///
/// # Safety
///
/// `doc` must be a live document handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_document_collection_timeline_at(
	doc: *const QuercusOtioDocument,
	index: i32,
) -> *mut QuercusOtioTimeline {
	if doc.is_null() || index < 0 {
		return std::ptr::null_mut();
	}
	let root = lock(&unsafe { &*doc }.inner);
	let Some(collection) = root.as_collection() else {
		return std::ptr::null_mut();
	};
	let timeline = collection
		.children()
		.iter()
		.filter_map(|s| s.as_timeline())
		.nth(index as usize)
		.cloned();
	match timeline {
		Some(tl) => {
			drop(root);
			crate::handle::into_ffi(QuercusOtioTimeline {
				inner: Mutex::new(tl),
			}) as *mut QuercusOtioTimeline
		}
		None => std::ptr::null_mut(),
	}
}

/// Wraps a copy of a timeline as a document root.
///
/// This is the document-level counterpart of
/// `quercus_otio_document_timeline` and the way to serialize a timeline
/// built through the structured API: pass the result to
/// `quercus_otio_document_to_json`. The timeline is deep-cloned, so later
/// edits to `timeline` are not reflected in the document.
///
/// # Returns
///
/// A new document handle at ref count 1 whose kind is
/// [`QUERCUS_OTIO_DOC_TIMELINE`], or null when `timeline` is null.
///
/// # Safety
///
/// `timeline` must be a live timeline handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_document_from_timeline(
	timeline: *const QuercusOtioTimeline,
) -> *mut QuercusOtioDocument {
	if timeline.is_null() {
		return std::ptr::null_mut();
	}
	let tl = lock(&unsafe { &*timeline }.inner).clone();
	crate::handle::into_ffi(QuercusOtioDocument {
		inner: Mutex::new(Serializable::Timeline(tl)),
	}) as *mut QuercusOtioDocument
}

// ---------------------------------------------------------------------------
// Timelines (FCPXML document unit / export builder)
// ---------------------------------------------------------------------------

/// A new, empty timeline list (ref count 1).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_otio_timelines_new() -> *mut QuercusOtioTimelines {
	crate::handle::into_ffi(QuercusOtioTimelines {
		inner: Mutex::new(Vec::new()),
	}) as *mut QuercusOtioTimelines
}

/// Parses an FCPXML document into a timeline list.
///
/// `xml` is a borrowed `(ptr, len)` UTF-8 byte slice; one timeline is
/// produced per `<sequence>` element. The buffer is only read during the
/// call.
///
/// # Returns
///
/// A new list handle at ref count 1, or null when `xml` is null, is not valid
/// UTF-8, or fails to parse. The list may legitimately be empty when the
/// document parses but contains no sequence.
///
/// # Safety
///
/// `xml` must be null or point to `xml_len` readable bytes for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timelines_from_fcpxml(
	xml: *const u8,
	xml_len: usize,
) -> *mut QuercusOtioTimelines {
	let Some(xml) = (unsafe { str_arg(xml, xml_len) }) else {
		return std::ptr::null_mut();
	};
	match quercus_otio::from_fcpxml_string(xml) {
		Ok(timelines) => crate::handle::into_ffi(QuercusOtioTimelines {
			inner: Mutex::new(timelines),
		}) as *mut QuercusOtioTimelines,
		Err(_) => std::ptr::null_mut(),
	}
}

/// Serializes the timeline list to an FCPXML document, snprintf-style.
///
/// Every timeline in the list becomes one `<sequence>`. At most
/// `buf_len - 1` bytes plus a trailing NUL are written; pass
/// `buf = null, buf_len = 0` to query the required size.
///
/// # Returns
///
/// The byte length of the XML (excluding the NUL), which may exceed the
/// truncated write, or -1 when `timelines` is null or serialization fails.
///
/// # Safety
///
/// `timelines` must be a live list handle (or null); `buf` must be null or
/// point to `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timelines_to_fcpxml(
	timelines: *const QuercusOtioTimelines,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	if timelines.is_null() {
		return -1;
	}
	let list = lock(&unsafe { &*timelines }.inner);
	match quercus_otio::to_fcpxml_string(&list) {
		Ok(xml) => {
			drop(list);
			copy_str_out(&xml, buf, buf_len)
		}
		Err(_) => -1,
	}
}

/// Counts the timelines in a list.
///
/// The count is the index range accepted by `quercus_otio_timelines_at` and
/// `quercus_otio_timelines_set`.
///
/// # Returns
///
/// The number of timelines, or -1 when `timelines` is null.
///
/// # Safety
///
/// `timelines` must be a live list handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timelines_count(timelines: *const QuercusOtioTimelines) -> i32 {
	if timelines.is_null() {
		return -1;
	}
	lock(&unsafe { &*timelines }.inner).len() as i32
}

/// Clones the timeline at `index` out of the list.
///
/// `index` is 0-based. The result is detached; edit it and write it back
/// with `quercus_otio_timelines_set` if the list should change.
///
/// # Returns
///
/// A new timeline handle at ref count 1, or null when `timelines` is null,
/// `index` is negative, or `index` is not a valid position in the list.
///
/// # Safety
///
/// `timelines` must be a live list handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timelines_at(
	timelines: *const QuercusOtioTimelines,
	index: i32,
) -> *mut QuercusOtioTimeline {
	if timelines.is_null() || index < 0 {
		return std::ptr::null_mut();
	}
	let list = lock(&unsafe { &*timelines }.inner);
	let Some(tl) = list.get(index as usize).cloned() else {
		return std::ptr::null_mut();
	};
	drop(list);
	crate::handle::into_ffi(QuercusOtioTimeline {
		inner: Mutex::new(tl),
	}) as *mut QuercusOtioTimeline
}

/// Replaces the timeline at `index` with a deep copy of `timeline`.
///
/// This is the write-back half of the traverse-edit-write pattern: clone out
/// with `quercus_otio_timelines_at`, edit the clone, then call this. The
/// list keeps its length and `timeline` itself is unchanged.
///
/// # Returns
///
/// `true` on success, `false` when `timelines` or `timeline` is null, `index`
/// is negative, or `index` is past the end of the list.
///
/// # Safety
///
/// `timelines` and `timeline` must be live handles of their respective types
/// (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timelines_set(
	timelines: *mut QuercusOtioTimelines,
	index: i32,
	timeline: *const QuercusOtioTimeline,
) -> bool {
	if timelines.is_null() || timeline.is_null() || index < 0 {
		return false;
	}
	let tl = lock(&unsafe { &*timeline }.inner).clone();
	let mut list = lock(&unsafe { &*timelines }.inner);
	let Some(slot) = list.get_mut(index as usize) else {
		return false;
	};
	*slot = tl;
	true
}

/// Appends a deep copy of `timeline` to the end of the list.
///
/// Grows the list by one; the stored copy is independent of `timeline`,
/// which the caller still owns. Pairs with `quercus_otio_timelines_new` when
/// assembling an FCPXML export.
///
/// # Returns
///
/// `true` on success, `false` when either handle is null.
///
/// # Safety
///
/// Both pointers must be live handles of their respective types (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timelines_add(
	timelines: *mut QuercusOtioTimelines,
	timeline: *const QuercusOtioTimeline,
) -> bool {
	if timelines.is_null() || timeline.is_null() {
		return false;
	}
	let tl = lock(&unsafe { &*timeline }.inner).clone();
	lock(&unsafe { &*timelines }.inner).push(tl);
	true
}

// ---------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------

/// Creates a named timeline with an empty tracks stack.
///
/// `name` is a borrowed `(ptr, len)` UTF-8 byte slice; it is copied, so the
/// caller may free the buffer afterwards. It may be empty, but the pointer
/// must be non-null.
///
/// # Returns
///
/// A new timeline handle at ref count 1, or null when `name` is null or not
/// valid UTF-8.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timeline_new(
	name: *const u8,
	name_len: usize,
) -> *mut QuercusOtioTimeline {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return std::ptr::null_mut();
	};
	crate::handle::into_ffi(QuercusOtioTimeline {
		inner: Mutex::new(Timeline::new(name)),
	}) as *mut QuercusOtioTimeline
}

/// Copies the timeline's name into `buf`, snprintf-style.
///
/// At most `buf_len - 1` bytes plus a trailing NUL are written; pass
/// `buf = null, buf_len = 0` to query the required size.
///
/// # Returns
///
/// The byte length of the name (excluding the NUL), which may exceed the
/// truncated write, or -1 when `timeline` is null.
///
/// # Safety
///
/// `timeline` must be a live timeline handle (or null); `buf` must be null
/// or point to `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timeline_name(
	timeline: *const QuercusOtioTimeline,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	if timeline.is_null() {
		return -1;
	}
	let name = lock(&unsafe { &*timeline }.inner).name().to_string();
	copy_str_out(&name, buf, buf_len)
}

/// Renames the timeline.
///
/// `name` is a borrowed `(ptr, len)` UTF-8 slice that is copied into the
/// timeline; on `false` the existing name is left untouched.
///
/// # Returns
///
/// `true` on success, `false` when `timeline` is null or `name` is null / not
/// valid UTF-8.
///
/// # Safety
///
/// `timeline` must be a live timeline handle (or null); `name` must be null
/// or point to `name_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timeline_set_name(
	timeline: *mut QuercusOtioTimeline,
	name: *const u8,
	name_len: usize,
) -> bool {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return false;
	};
	if timeline.is_null() {
		return false;
	}
	lock(&unsafe { &*timeline }.inner).set_name(name);
	true
}

/// Reads the timeline's global start time into `out`.
///
/// The value is a [`QuercusOtioRationalTime`] (frame value + rate) mirroring
/// the OTIO `global_start_time` field. `out` is only written on success.
///
/// # Returns
///
/// `true` when a start time was copied, `false` when `timeline` or `out` is
/// null, or the timeline has no global start time set.
///
/// # Safety
///
/// `timeline` must be a live handle (or null); `out` must be null or point to
/// a writable [`QuercusOtioRationalTime`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timeline_global_start_time(
	timeline: *const QuercusOtioTimeline,
	out: *mut QuercusOtioRationalTime,
) -> bool {
	if timeline.is_null() || out.is_null() {
		return false;
	}
	let tl = lock(&unsafe { &*timeline }.inner);
	let Some(start) = tl.global_start_time() else {
		return false;
	};
	unsafe { *out = QuercusOtioRationalTime::from_native(&start) };
	true
}

/// Sets the timeline's global start time.
///
/// `start` is passed by value as a `RationalTime` (frame value + rate) and
/// copied; read it back with `quercus_otio_timeline_global_start_time`. No
/// range or rate validation is performed.
///
/// # Returns
///
/// `true` on success, `false` when `timeline` is null.
///
/// # Safety
///
/// `timeline` must be a live timeline handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timeline_set_global_start_time(
	timeline: *mut QuercusOtioTimeline,
	start: QuercusOtioRationalTime,
) -> bool {
	if timeline.is_null() {
		return false;
	}
	lock(&unsafe { &*timeline }.inner).set_global_start_time(start.to_native());
	true
}

/// Serializes this timeline alone to OTIO JSON, snprintf-style.
///
/// Unlike `quercus_otio_document_to_json` no document wrapper is involved:
/// the timeline is emitted as the root `Timeline` object. At most
/// `buf_len - 1` bytes plus a trailing NUL are written; `buf = null,
/// buf_len = 0` queries the required size.
///
/// # Returns
///
/// The byte length of the JSON (excluding the NUL), which may exceed the
/// truncated write, or -1 when `timeline` is null or serialization fails.
///
/// # Safety
///
/// `timeline` must be a live handle (or null); `buf` must be null or point to
/// `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timeline_to_json(
	timeline: *const QuercusOtioTimeline,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	if timeline.is_null() {
		return -1;
	}
	let tl = lock(&unsafe { &*timeline }.inner);
	match tl.to_json_string() {
		Ok(json) => {
			drop(tl);
			copy_str_out(&json, buf, buf_len)
		}
		Err(_) => -1,
	}
}

/// Counts the tracks in the timeline's tracks stack.
///
/// The count is the index range accepted by
/// `quercus_otio_timeline_track_at`, `_set_track` and `_remove_track`.
///
/// # Returns
///
/// The number of track children, or -1 when `timeline` is null.
///
/// # Safety
///
/// `timeline` must be a live timeline handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timeline_track_count(timeline: *const QuercusOtioTimeline) -> i32 {
	if timeline.is_null() {
		return -1;
	}
	lock(&unsafe { &*timeline }.inner).tracks().children().len() as i32
}

/// Clones the track at `index` out of the timeline's tracks stack.
///
/// `index` is 0-based over all stack children; a child that is not a track
/// (for example a nested stack) yields null. Edit the clone and store it back
/// with `quercus_otio_timeline_set_track`.
///
/// # Returns
///
/// A new track handle at ref count 1, or null when `timeline` is null,
/// `index` is negative, is past the end, or the child is not a track.
///
/// # Safety
///
/// `timeline` must be a live timeline handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timeline_track_at(
	timeline: *const QuercusOtioTimeline,
	index: i32,
) -> *mut QuercusOtioTrack {
	if timeline.is_null() || index < 0 {
		return std::ptr::null_mut();
	}
	let tl = lock(&unsafe { &*timeline }.inner);
	let track = tl
		.tracks()
		.children()
		.get(index as usize)
		.and_then(|c| c.as_track())
		.cloned();
	match track {
		Some(track) => {
			drop(tl);
			crate::handle::into_ffi(QuercusOtioTrack {
				inner: Mutex::new(track),
			}) as *mut QuercusOtioTrack
		}
		None => std::ptr::null_mut(),
	}
}

/// Replaces the track at `index` with a deep copy of `track`.
///
/// Write-back half of the traverse-edit-write pattern for timeline stacks,
/// paired with `quercus_otio_timeline_track_at` (use
/// `quercus_otio_timeline_append_track` when only appending). `track` is
/// unchanged.
///
/// # Returns
///
/// `true` on success, `false` when either handle is null, `index` is negative,
/// or `index` is past the end of the tracks stack.
///
/// # Safety
///
/// Both pointers must be live handles of their respective types (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timeline_set_track(
	timeline: *mut QuercusOtioTimeline,
	index: i32,
	track: *const QuercusOtioTrack,
) -> bool {
	if timeline.is_null() || track.is_null() || index < 0 {
		return false;
	}
	let track = lock(&unsafe { &*track }.inner).clone();
	lock(&unsafe { &*timeline }.inner)
		.tracks_mut()
		.set_child(index as usize, Composable::Track(track))
}

/// Appends a deep copy of `track` to the timeline's tracks stack.
///
/// Used when building a timeline from scratch (imported interchange document
/// or a generated rough cut). The stored copy is independent of `track`,
/// which the caller still owns.
///
/// # Returns
///
/// `true` on success, `false` when either handle is null.
///
/// # Safety
///
/// Both pointers must be live handles of their respective types (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timeline_append_track(
	timeline: *mut QuercusOtioTimeline,
	track: *const QuercusOtioTrack,
) -> bool {
	if timeline.is_null() || track.is_null() {
		return false;
	}
	let track = lock(&unsafe { &*track }.inner).clone();
	lock(&unsafe { &*timeline }.inner)
		.tracks_mut()
		.append_child(Composable::Track(track));
	true
}

/// Removes the track at `index` from the timeline's tracks stack.
///
/// Later tracks shift down by one, so indices above `index` change.
///
/// # Returns
///
/// `true` on success, `false` when `timeline` is null, `index` is negative, or
/// `index` is past the end.
///
/// # Safety
///
/// `timeline` must be a live timeline handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_timeline_remove_track(
	timeline: *mut QuercusOtioTimeline,
	index: i32,
) -> bool {
	if timeline.is_null() || index < 0 {
		return false;
	}
	lock(&unsafe { &*timeline }.inner)
		.tracks_mut()
		.remove_child(index as usize)
}

// ---------------------------------------------------------------------------
// Track
// ---------------------------------------------------------------------------

/// Creates an empty track of the given kind.
///
/// `kind` is a borrowed `(ptr, len)` UTF-8 slice such as `"Video"` or
/// `"Audio"`; it is copied into the OTIO `kind` field, so the caller may free
/// the buffer afterwards. No validation of the kind string is performed.
///
/// # Returns
///
/// A new track handle at ref count 1, or null when `kind` is null or not valid
/// UTF-8.
///
/// # Safety
///
/// `kind` must be null or point to `kind_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_track_new(kind: *const u8, kind_len: usize) -> *mut QuercusOtioTrack {
	let Some(kind) = (unsafe { str_arg(kind, kind_len) }) else {
		return std::ptr::null_mut();
	};
	crate::handle::into_ffi(QuercusOtioTrack {
		inner: Mutex::new(Track::new(kind)),
	}) as *mut QuercusOtioTrack
}

/// Copies the track's kind (`"Video"`, `"Audio"`, ...) into `buf`,
/// snprintf-style.
///
/// At most `buf_len - 1` bytes plus a trailing NUL are written; pass
/// `buf = null, buf_len = 0` to query the required size.
///
/// # Returns
///
/// The byte length of the kind (excluding the NUL), or -1 when `track` is
/// null.
///
/// # Safety
///
/// `track` must be a live handle (or null); `buf` must be null or point to
/// `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_track_kind(
	track: *const QuercusOtioTrack,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	if track.is_null() {
		return -1;
	}
	let kind = lock(&unsafe { &*track }.inner).kind().to_string();
	copy_str_out(&kind, buf, buf_len)
}

/// Counts the children (clips, gaps and transitions) of a track.
///
/// The count is the index range accepted by `quercus_otio_track_child_at`,
/// `_set_child` and `_remove_child`.
///
/// # Returns
///
/// The number of children, or -1 when `track` is null.
///
/// # Safety
///
/// `track` must be a live track handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_track_child_count(track: *const QuercusOtioTrack) -> i32 {
	if track.is_null() {
		return -1;
	}
	lock(&unsafe { &*track }.inner).children().len() as i32
}

/// Clones the child at `index` out of the track.
///
/// `index` is 0-based over clips, gaps and transitions in track order. The
/// result is detached; edit it and store it back with
/// `quercus_otio_track_set_child`.
///
/// # Returns
///
/// A new composable handle at ref count 1, or null when `track` is null,
/// `index` is negative, or `index` is past the last child.
///
/// # Safety
///
/// `track` must be a live track handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_track_child_at(
	track: *const QuercusOtioTrack,
	index: i32,
) -> *mut QuercusOtioComposable {
	if track.is_null() || index < 0 {
		return std::ptr::null_mut();
	}
	let track = lock(&unsafe { &*track }.inner);
	let Some(child) = track.children().get(index as usize).map(|c| (**c).clone()) else {
		return std::ptr::null_mut();
	};
	drop(track);
	crate::handle::into_ffi(QuercusOtioComposable {
		inner: Mutex::new(child),
	}) as *mut QuercusOtioComposable
}

/// Replaces the child at `index` with a deep copy of `child`.
///
/// Write-back half of the traverse-edit-write pattern for a track, paired
/// with `quercus_otio_track_child_at` (use `quercus_otio_track_append_child`
/// when only appending). `child` itself is unchanged.
///
/// # Returns
///
/// `true` on success, `false` when either handle is null, `index` is negative,
/// or `index` is past the end of the track.
///
/// # Safety
///
/// Both pointers must be live handles of their respective types (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_track_set_child(
	track: *mut QuercusOtioTrack,
	index: i32,
	child: *const QuercusOtioComposable,
) -> bool {
	if track.is_null() || child.is_null() || index < 0 {
		return false;
	}
	let child = lock(&unsafe { &*child }.inner).clone();
	lock(&unsafe { &*track }.inner).set_child(index as usize, child)
}

/// Appends a deep copy of `child` to the end of the track.
///
/// This is how a track is built from clips, gaps and transitions. The stored
/// copy is independent of `child`, which the caller still owns and must
/// release.
///
/// # Returns
///
/// `true` on success, `false` when either handle is null.
///
/// # Safety
///
/// Both pointers must be live handles of their respective types (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_track_append_child(
	track: *mut QuercusOtioTrack,
	child: *const QuercusOtioComposable,
) -> bool {
	if track.is_null() || child.is_null() {
		return false;
	}
	let child = lock(&unsafe { &*child }.inner).clone();
	lock(&unsafe { &*track }.inner).append_child(child);
	true
}

/// Removes the child at `index` from the track.
///
/// Later children shift down by one, so indices above `index` change.
///
/// # Returns
///
/// `true` on success, `false` when `track` is null, `index` is negative, or
/// `index` is past the end.
///
/// # Safety
///
/// `track` must be a live track handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_track_remove_child(track: *mut QuercusOtioTrack, index: i32) -> bool {
	if track.is_null() || index < 0 {
		return false;
	}
	lock(&unsafe { &*track }.inner).remove_child(index as usize)
}

// ---------------------------------------------------------------------------
// Composable (clip / gap / transition)
// ---------------------------------------------------------------------------

/// Classifies a composable handle.
///
/// # Returns
///
/// [`QUERCUS_OTIO_COMPOSABLE_CLIP`], [`QUERCUS_OTIO_COMPOSABLE_GAP`],
/// [`QUERCUS_OTIO_COMPOSABLE_TRANSITION`], or
/// [`QUERCUS_OTIO_COMPOSABLE_OTHER`] for nested tracks/stacks and
/// unrecognized schemas; -1 when `c` is null.
///
/// # Safety
///
/// `c` must be a live composable handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_composable_kind(c: *const QuercusOtioComposable) -> i32 {
	if c.is_null() {
		return -1;
	}
	match &*lock(&unsafe { &*c }.inner) {
		Composable::Clip(_) => QUERCUS_OTIO_COMPOSABLE_CLIP,
		Composable::Gap(_) => QUERCUS_OTIO_COMPOSABLE_GAP,
		Composable::Transition(_) => QUERCUS_OTIO_COMPOSABLE_TRANSITION,
		_ => QUERCUS_OTIO_COMPOSABLE_OTHER,
	}
}

/// Copies the composable's name into `buf`, snprintf-style.
///
/// Works for clips, gaps and transitions; blocks kept whole as unrecognized
/// schemas report an empty string. At most `buf_len - 1` bytes plus a
/// trailing NUL are written; `buf = null, buf_len = 0` queries the required
/// size.
///
/// # Returns
///
/// The byte length of the name (excluding the NUL), or -1 when `c` is null.
///
/// # Safety
///
/// `c` must be a live handle (or null); `buf` must be null or point to
/// `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_composable_name(
	c: *const QuercusOtioComposable,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	if c.is_null() {
		return -1;
	}
	let name = lock(&unsafe { &*c }.inner).name().to_string();
	copy_str_out(&name, buf, buf_len)
}

/// Reads the source range of a clip or gap into `out`.
///
/// `out` receives start time and duration as a [`QuercusOtioTimeRange`] and
/// is only written on success. Transitions (which carry in/out offsets
/// instead) and other kinds are rejected.
///
/// # Returns
///
/// `true` when a range was copied, `false` when `c` or `out` is null, the
/// composable is not a clip/gap, or its source range is unset.
///
/// # Safety
///
/// `c` must be a live handle (or null); `out` must be null or point to a
/// writable [`QuercusOtioTimeRange`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_composable_source_range(
	c: *const QuercusOtioComposable,
	out: *mut QuercusOtioTimeRange,
) -> bool {
	if c.is_null() || out.is_null() {
		return false;
	}
	let c = lock(&unsafe { &*c }.inner);
	let Some(range) = c.source_range() else {
		return false;
	};
	unsafe { *out = QuercusOtioTimeRange::from_native(&range) };
	true
}

/// Creates a clip, optionally with an external media reference and a source
/// range.
///
/// `name` is a borrowed `(ptr, len)` UTF-8 slice and is required (non-null;
/// copied). `url` is an optional borrowed `(ptr, len)` slice: null or empty
/// leaves the clip with a missing media reference, otherwise the value
/// becomes the target of an `ExternalReference`. `source_range` is an
/// optional pointer: null leaves the range unset, otherwise the pointed-to
/// range is copied.
///
/// # Returns
///
/// A new composable handle holding a `Clip` at ref count 1, or null when
/// `name` is missing/invalid UTF-8, or `url` is non-null but not valid UTF-8.
///
/// # Safety
///
/// `name` and `url` must be null or point to their stated lengths of readable
/// bytes; `source_range` must be null or point to a valid
/// [`QuercusOtioTimeRange`]. All inputs are only read during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_clip_new(
	name: *const u8,
	name_len: usize,
	url: *const u8,
	url_len: usize,
	source_range: *const QuercusOtioTimeRange,
) -> *mut QuercusOtioComposable {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return std::ptr::null_mut();
	};
	let mut clip = Clip::new(name);
	if let Some(url) = unsafe { str_arg(url, url_len) }.filter(|u| !u.is_empty()) {
		clip.set_media_reference(MediaReference::ExternalReference(ExternalReference::new(
			url, None,
		)));
	}
	if !source_range.is_null() {
		clip.set_source_range(unsafe { &*source_range }.to_native());
	}
	crate::handle::into_ffi(QuercusOtioComposable {
		inner: Mutex::new(Composable::Clip(clip)),
	}) as *mut QuercusOtioComposable
}

/// Creates a gap: a named empty span of the given source range.
///
/// `name` is a borrowed `(ptr, len)` UTF-8 slice and is required (non-null;
/// copied). `source_range` is optional: null yields the default zero-length
/// range, which is valid but contributes no time to the track.
///
/// # Returns
///
/// A new composable handle holding a `Gap` at ref count 1, or null when `name`
/// is null or not valid UTF-8.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes; `source_range`
/// must be null or point to a valid [`QuercusOtioTimeRange`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_gap_new(
	name: *const u8,
	name_len: usize,
	source_range: *const QuercusOtioTimeRange,
) -> *mut QuercusOtioComposable {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return std::ptr::null_mut();
	};
	let range = if source_range.is_null() {
		TimeRange::default()
	} else {
		unsafe { &*source_range }.to_native()
	};
	crate::handle::into_ffi(QuercusOtioComposable {
		inner: Mutex::new(Composable::Gap(Gap::new(range, name))),
	}) as *mut QuercusOtioComposable
}

/// Creates a transition with the given in/out offsets.
///
/// `name` is a borrowed `(ptr, len)` UTF-8 slice and is required (non-null;
/// copied). `in_offset` and `out_offset` are passed by value as `RationalTime`
/// values and describe how far the transition extends into the neighbouring
/// clips on each side. The transition type is not settable here; read it back
/// with `quercus_otio_transition_type`.
///
/// # Returns
///
/// A new composable handle holding a `Transition` at ref count 1, or null when
/// `name` is null or not valid UTF-8.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_transition_new(
	name: *const u8,
	name_len: usize,
	in_offset: QuercusOtioRationalTime,
	out_offset: QuercusOtioRationalTime,
) -> *mut QuercusOtioComposable {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return std::ptr::null_mut();
	};
	let mut transition = Transition::new(name);
	transition.set_in_offset(in_offset.to_native());
	transition.set_out_offset(out_offset.to_native());
	crate::handle::into_ffi(QuercusOtioComposable {
		inner: Mutex::new(Composable::Transition(transition)),
	}) as *mut QuercusOtioComposable
}

/// Sets or replaces the clip's source range.
///
/// `range` is passed by value (start time + duration) and copied; it selects
/// which part of the media the clip shows. No validation of the range against
/// the media duration is performed.
///
/// # Returns
///
/// `true` on success, `false` when `c` is null or the composable is not a
/// clip.
///
/// # Safety
///
/// `c` must be a live composable handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_clip_set_source_range(
	c: *mut QuercusOtioComposable,
	range: QuercusOtioTimeRange,
) -> bool {
	if c.is_null() {
		return false;
	}
	match &mut *lock(&unsafe { &*c }.inner) {
		Composable::Clip(clip) => {
			clip.set_source_range(range.to_native());
			true
		}
		_ => false,
	}
}

/// Copies the clip's external media URL into `buf`, snprintf-style.
///
/// At most `buf_len - 1` bytes plus a trailing NUL are written; pass
/// `buf = null, buf_len = 0` to query the required size. A clip whose media
/// reference is missing or not external produces -1 rather than an empty
/// string.
///
/// # Returns
///
/// The byte length of the URL (excluding the NUL), or -1 when `c` is null,
/// the composable is not a clip, or the media reference is not an
/// `ExternalReference`.
///
/// # Safety
///
/// `c` must be a live handle (or null); `buf` must be null or point to
/// `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_clip_media_url(
	c: *const QuercusOtioComposable,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	if c.is_null() {
		return -1;
	}
	let c = lock(&unsafe { &*c }.inner);
	let url = match &*c {
		Composable::Clip(clip) => match clip.media_reference() {
			Some(MediaReference::ExternalReference(r)) => Some(r.target_url().to_string()),
			_ => None,
		},
		_ => None,
	};
	match url {
		Some(url) => {
			drop(c);
			copy_str_out(&url, buf, buf_len)
		}
		None => -1,
	}
}

/// Points the clip's media reference at an external URL.
///
/// `url` is a borrowed `(ptr, len)` UTF-8 slice; it is copied into a new
/// `ExternalReference`, replacing whatever reference the clip had. An empty
/// `url` is accepted and stored as an empty target - it does not clear the
/// reference.
///
/// # Returns
///
/// `true` on success, `false` when `c` is null, `url` is null or not valid
/// UTF-8, or the composable is not a clip.
///
/// # Safety
///
/// `c` must be a live composable handle (or null); `url` must be null or
/// point to `url_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_clip_set_media_url(
	c: *mut QuercusOtioComposable,
	url: *const u8,
	url_len: usize,
) -> bool {
	let Some(url) = (unsafe { str_arg(url, url_len) }) else {
		return false;
	};
	if c.is_null() {
		return false;
	}
	match &mut *lock(&unsafe { &*c }.inner) {
		Composable::Clip(clip) => {
			clip.set_media_reference(MediaReference::ExternalReference(ExternalReference::new(
				url, None,
			)));
			true
		}
		_ => false,
	}
}

/// Reports whether the clip is enabled, i.e. contributes to the track's
/// output.
///
/// # Returns
///
/// 1 when enabled, 0 when disabled, or -1 when `c` is null or the composable
/// is not a clip. Pairs with `quercus_otio_clip_set_enabled`.
///
/// # Safety
///
/// `c` must be a live composable handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_clip_enabled(c: *const QuercusOtioComposable) -> i32 {
	if c.is_null() {
		return -1;
	}
	match &*lock(&unsafe { &*c }.inner) {
		Composable::Clip(clip) => clip.enabled() as i32,
		_ => -1,
	}
}

/// Sets the clip's enabled flag.
///
/// A disabled clip stays in the track but is imported/exported with
/// `enabled: false`. Counterpart of `quercus_otio_clip_enabled`.
///
/// # Returns
///
/// `true` on success, `false` when `c` is null or the composable is not a
/// clip.
///
/// # Safety
///
/// `c` must be a live composable handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_clip_set_enabled(
	c: *mut QuercusOtioComposable,
	enabled: bool,
) -> bool {
	if c.is_null() {
		return false;
	}
	match &mut *lock(&unsafe { &*c }.inner) {
		Composable::Clip(clip) => {
			clip.set_enabled(enabled);
			true
		}
		_ => false,
	}
}

/// Copies the transition type (`"SMPTE_Dissolve"`, ...) into `buf`,
/// snprintf-style.
///
/// At most `buf_len - 1` bytes plus a trailing NUL are written; pass
/// `buf = null, buf_len = 0` to query the required size. The type is
/// read-only through this ABI: there is no setter.
///
/// # Returns
///
/// The byte length of the type string (excluding the NUL), or -1 when `c` is
/// null or the composable is not a transition.
///
/// # Safety
///
/// `c` must be a live handle (or null); `buf` must be null or point to
/// `buf_len` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_transition_type(
	c: *const QuercusOtioComposable,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	if c.is_null() {
		return -1;
	}
	let c = lock(&unsafe { &*c }.inner);
	match &*c {
		Composable::Transition(t) => {
			let ty = t.transition_type().to_string();
			drop(c);
			copy_str_out(&ty, buf, buf_len)
		}
		_ => -1,
	}
}

/// Reads one of a transition's offsets into `out`.
///
/// `is_in` selects the side: `true` reads the in-offset, `false` the
/// out-offset. `out` receives a [`QuercusOtioRationalTime`] and is only
/// written on success. Counterpart of
/// `quercus_otio_transition_set_offset`.
///
/// # Returns
///
/// `true` when an offset was copied, `false` when `c` or `out` is null or the
/// composable is not a transition.
///
/// # Safety
///
/// `c` must be a live handle (or null); `out` must be null or point to a
/// writable [`QuercusOtioRationalTime`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_transition_offset(
	c: *const QuercusOtioComposable,
	is_in: bool,
	out: *mut QuercusOtioRationalTime,
) -> bool {
	if c.is_null() || out.is_null() {
		return false;
	}
	let c = lock(&unsafe { &*c }.inner);
	let Composable::Transition(t) = &*c else {
		return false;
	};
	let offset = if is_in { t.in_offset() } else { t.out_offset() };
	unsafe { *out = QuercusOtioRationalTime::from_native(&offset) };
	true
}

/// Sets one of a transition's offsets.
///
/// `is_in` selects the side (`true` = in-offset, `false` = out-offset);
/// `offset` is passed by value as a `RationalTime` and copied. Counterpart of
/// `quercus_otio_transition_offset`.
///
/// # Returns
///
/// `true` on success, `false` when `c` is null or the composable is not a
/// transition.
///
/// # Safety
///
/// `c` must be a live composable handle (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_otio_transition_set_offset(
	c: *mut QuercusOtioComposable,
	is_in: bool,
	offset: QuercusOtioRationalTime,
) -> bool {
	if c.is_null() {
		return false;
	}
	match &mut *lock(&unsafe { &*c }.inner) {
		Composable::Transition(t) => {
			if is_in {
				t.set_in_offset(offset.to_native());
			} else {
				t.set_out_offset(offset.to_native());
			}
			true
		}
		_ => false,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn arg(s: &str) -> (*const u8, usize) {
		(s.as_ptr(), s.len())
	}

	/// Builds a one-track timeline (clip + gap) through the C ABI and
	/// reads it back through the document layer.
	#[test]
	fn build_traverse_and_json_roundtrip() {
		unsafe {
			let (n, nl) = arg("Rough Cut");
			let tl = quercus_otio_timeline_new(n, nl);
			assert!(!tl.is_null());
			assert!(quercus_otio_timeline_set_global_start_time(
				tl,
				QuercusOtioRationalTime {
					value: 0.0,
					rate: 24.0
				}
			));

			let (k, kl) = arg("Video");
			let track = quercus_otio_track_new(k, kl);
			let range = QuercusOtioTimeRange {
				start_time: QuercusOtioRationalTime {
					value: 10.0,
					rate: 24.0,
				},
				duration: QuercusOtioRationalTime {
					value: 48.0,
					rate: 24.0,
				},
			};
			let (cn, cnl) = arg("clip1");
			let (u, ul) = arg("file:///tmp/a.mov");
			let clip = quercus_otio_clip_new(cn, cnl, u, ul, &range);
			assert!(!clip.is_null());
			assert_eq!(quercus_otio_composable_kind(clip), QUERCUS_OTIO_COMPOSABLE_CLIP);
			assert!(quercus_otio_track_append_child(track, clip));
			quercus_otio_composable_release(clip);

			let (gn, gnl) = arg("gap");
			let gap = quercus_otio_gap_new(gn, gnl, std::ptr::null());
			assert!(quercus_otio_track_append_child(track, gap));
			quercus_otio_composable_release(gap);

			assert!(quercus_otio_timeline_append_track(tl, track));
			quercus_otio_track_release(track);

			// Traverse back.
			assert_eq!(quercus_otio_timeline_track_count(tl), 1);
			let track = quercus_otio_timeline_track_at(tl, 0);
			assert!(!track.is_null());
			assert_eq!(quercus_otio_track_child_count(track), 2);
			let child = quercus_otio_track_child_at(track, 0);
			assert_eq!(quercus_otio_composable_kind(child), QUERCUS_OTIO_COMPOSABLE_CLIP);
			let mut buf = [0u8; 64];
			let n = quercus_otio_clip_media_url(child, buf.as_mut_ptr(), buf.len());
			assert_eq!(
				std::str::from_utf8(&buf[..n as usize]).unwrap(),
				"file:///tmp/a.mov"
			);
			let mut got = QuercusOtioTimeRange::default();
			assert!(quercus_otio_composable_source_range(child, &mut got));
			assert_eq!(got.duration.value, 48.0);
			quercus_otio_composable_release(child);
			quercus_otio_track_release(track);

			// JSON roundtrip via the document layer.
			let doc = quercus_otio_document_from_timeline(tl);
			let len = quercus_otio_document_to_json(doc, std::ptr::null_mut(), 0);
			assert!(len > 0);
			let mut buf = vec![0u8; len as usize + 1];
			assert_eq!(
				quercus_otio_document_to_json(doc, buf.as_mut_ptr(), buf.len()),
				len
			);
			buf.truncate(len as usize);
			quercus_otio_document_release(doc);
			quercus_otio_timeline_release(tl);

			let doc2 = quercus_otio_document_from_json(buf.as_ptr(), buf.len());
			assert!(!doc2.is_null());
			assert_eq!(quercus_otio_document_kind(doc2), QUERCUS_OTIO_DOC_TIMELINE);
			let tl2 = quercus_otio_document_timeline(doc2);
			assert!(!tl2.is_null());
			let n = quercus_otio_timeline_name(tl2, buf.as_mut_ptr(), buf.len());
			assert_eq!(
				std::str::from_utf8(&buf[..n as usize]).unwrap(),
				"Rough Cut"
			);
			quercus_otio_timeline_release(tl2);
			quercus_otio_document_release(doc2);
		}
	}

	/// FCPXML: build → serialize → parse → the structure survives.
	#[test]
	fn fcpxml_roundtrip() {
		unsafe {
			let (n, nl) = arg("FCP Sequence");
			let tl = quercus_otio_timeline_new(n, nl);
			let (k, kl) = arg("Video");
			let track = quercus_otio_track_new(k, kl);
			let range = QuercusOtioTimeRange {
				start_time: QuercusOtioRationalTime {
					value: 0.0,
					rate: 25.0,
				},
				duration: QuercusOtioRationalTime {
					value: 50.0,
					rate: 25.0,
				},
			};
			let (cn, cnl) = arg("shot");
			let (u, ul) = arg("file:///tmp/shot.mov");
			let clip = quercus_otio_clip_new(cn, cnl, u, ul, &range);
			assert!(quercus_otio_track_append_child(track, clip));
			assert!(quercus_otio_timeline_append_track(tl, track));

			let list = quercus_otio_timelines_new();
			assert!(quercus_otio_timelines_add(list, tl));
			let len = quercus_otio_timelines_to_fcpxml(list, std::ptr::null_mut(), 0);
			assert!(len > 0);
			let mut xml = vec![0u8; len as usize + 1];
			assert_eq!(
				quercus_otio_timelines_to_fcpxml(list, xml.as_mut_ptr(), xml.len()),
				len
			);
			xml.truncate(len as usize);

			let back = quercus_otio_timelines_from_fcpxml(xml.as_ptr(), xml.len());
			assert!(!back.is_null());
			assert_eq!(quercus_otio_timelines_count(back), 1);
			let tl2 = quercus_otio_timelines_at(back, 0);
			let mut buf = [0u8; 64];
			let n = quercus_otio_timeline_name(tl2, buf.as_mut_ptr(), buf.len());
			assert_eq!(
				std::str::from_utf8(&buf[..n as usize]).unwrap(),
				"FCP Sequence"
			);
			assert_eq!(quercus_otio_timeline_track_count(tl2), 1);

			quercus_otio_timeline_release(tl2);
			quercus_otio_timelines_release(back);
			quercus_otio_composable_release(clip);
			quercus_otio_track_release(track);
			quercus_otio_timeline_release(tl);
			quercus_otio_timelines_release(list);
		}
	}

	#[test]
	fn null_and_invalid_input_fail_closed() {
		unsafe {
			assert!(quercus_otio_document_from_json(std::ptr::null(), 0).is_null());
			assert!(quercus_otio_document_from_json("{".as_ptr(), 1).is_null());
			assert!(quercus_otio_timelines_from_fcpxml("nope".as_ptr(), 4).is_null());
			assert_eq!(quercus_otio_document_kind(std::ptr::null()), -1);
			assert_eq!(quercus_otio_timeline_track_count(std::ptr::null()), -1);
			assert!(quercus_otio_document_timeline(std::ptr::null()).is_null());
			assert!(
				quercus_otio_clip_new(std::ptr::null(), 0, std::ptr::null(), 0, std::ptr::null())
					.is_null()
			);
			// Unknown kind: media URL is only for clips.
			let (n, nl) = arg("t");
			let tr = quercus_otio_transition_new(
				n,
				nl,
				QuercusOtioRationalTime::default(),
				QuercusOtioRationalTime::default(),
			);
			assert_eq!(quercus_otio_clip_media_url(tr, std::ptr::null_mut(), 0), -1);
			assert!(!quercus_otio_clip_set_enabled(tr, false));
			quercus_otio_composable_release(tr);
		}
	}
}
