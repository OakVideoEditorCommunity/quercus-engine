//! Safe wrapper over the engine's document-model C ABI (`quercus_node_*`,
//! `quercus_project_*`, `quercus_undo_*`).
//!
//! Nodes are addressed by [`NodeId`] tokens (u64); their lifetime is the
//! project's, no handles to manage. The project itself is a refcounted
//! [`Project`] handle; plugins read the host's open project through
//! [`active_project`].

use std::ffi::c_void;

use crate::codec::string_out;
use crate::types::Rational;

#[cfg_attr(windows, link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim"))]
#[cfg_attr(not(windows), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_node_id_invalid() -> u64;
	fn quercus_node_id_is_valid(id: u64) -> bool;

	fn quercus_project_new() -> *mut c_void;
	fn quercus_project_set_active(project: *const c_void);
	fn quercus_project_active() -> *mut c_void;
	fn quercus_project_into_storage_handle(project: *mut c_void)
	-> crate::editor::StorageProjectHandle;
	fn quercus_project_name(project: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_project_filename(project: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_project_pretty_filename(project: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_project_modified(project: *const c_void) -> bool;
	fn quercus_project_root(project: *const c_void) -> u64;
	fn quercus_project_add_ref(project: *const c_void);
	fn quercus_project_release(project: *const c_void);
	fn quercus_project_uuid(project: *const c_void, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_project_setting(
		project: *const c_void,
		key: *const u8,
		key_len: usize,
		buf: *mut u8,
		buf_len: usize,
	) -> i32;
	fn quercus_project_set_setting(
		project: *const c_void,
		key: *const u8,
		key_len: usize,
		value: *const u8,
		value_len: usize,
	) -> bool;
	fn quercus_project_cache_location(
		project: *const c_void,
		setting: *mut i32,
		path: *mut u8,
		path_len: usize,
	) -> i32;
	fn quercus_project_set_cache_location(
		project: *const c_void,
		setting: i32,
		path: *const u8,
		path_len: usize,
	) -> bool;

	fn quercus_node_type_id(project: *const c_void, id: u64, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_node_kind(project: *const c_void, id: u64) -> i32;

	fn quercus_folder_child_count(project: *const c_void, id: u64) -> i32;
	fn quercus_folder_child_at(project: *const c_void, id: u64, index: i32) -> u64;

	fn quercus_footage_filename(project: *const c_void, id: u64, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_footage_total_stream_count(project: *const c_void, id: u64) -> i32;
	fn quercus_footage_video_stream_count(project: *const c_void, id: u64) -> i32;
	fn quercus_footage_audio_stream_count(project: *const c_void, id: u64) -> i32;
	fn quercus_footage_subtitle_stream_count(project: *const c_void, id: u64) -> i32;
	fn quercus_footage_duration(project: *const c_void, id: u64) -> Rational;
	fn quercus_footage_video_length(project: *const c_void, id: u64) -> Rational;
	fn quercus_footage_video_params(
		project: *const c_void,
		id: u64,
		ordinal: i32,
		out: *mut StreamVideoParams,
	) -> bool;
	fn quercus_footage_audio_params(
		project: *const c_void,
		id: u64,
		ordinal: i32,
		out: *mut StreamAudioParams,
	) -> bool;
	fn quercus_footage_custom_proxy_params(
		project: *mut c_void,
		id: u64,
		out: *mut crate::codec::ProxyParams,
	) -> bool;
	fn quercus_footage_effective_proxy_params(
		project: *mut c_void,
		id: u64,
		out: *mut crate::codec::ProxyParams,
	) -> bool;
	fn quercus_footage_source_start_time(
		project: *const c_void,
		footage: u64,
		out: *mut Rational,
	) -> bool;

	fn quercus_sequence_track_list_count(project: *const c_void, id: u64) -> i32;
	fn quercus_sequence_track_list_at(project: *const c_void, id: u64, index: i32) -> u64;
	fn quercus_sequence_playhead(project: *const c_void, id: u64) -> Rational;
	fn quercus_sequence_length(project: *const c_void, id: u64) -> Rational;
	fn quercus_sequence_frame_rate(project: *const c_void, id: u64) -> Rational;
	fn quercus_sequence_geometry(project: *const c_void, id: u64, width: *mut i32, height: *mut i32) -> bool;
	fn quercus_sequence_interlaced(project: *const c_void, id: u64) -> i32;
	fn quercus_sequence_set_video_params(
		project: *const c_void,
		id: u64,
		width: i32,
		height: i32,
		frame_rate_num: i32,
		frame_rate_den: i32,
	) -> bool;
	fn quercus_sequence_set_interlaced(project: *const c_void, id: u64, interlaced: bool) -> bool;
	fn quercus_node_label(project: *const c_void, id: u64, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_node_set_label(project: *const c_void, id: u64, label: *const u8, label_len: usize) -> bool;
	fn quercus_tracklist_kind(project: *const c_void, id: u64) -> i32;
	fn quercus_tracklist_track_count(project: *const c_void, id: u64) -> i32;
	fn quercus_tracklist_track_at(project: *const c_void, id: u64, index: i32) -> u64;
	fn quercus_track_kind(project: *const c_void, id: u64) -> i32;
	fn quercus_track_height(project: *const c_void, id: u64) -> f64;
	fn quercus_track_muted(project: *const c_void, id: u64) -> bool;
	fn quercus_track_locked(project: *const c_void, id: u64) -> bool;
	fn quercus_track_block_count(project: *const c_void, id: u64) -> i32;
	fn quercus_track_block_at(project: *const c_void, id: u64, index: i32) -> u64;
	fn quercus_track_block_containing_time(project: *const c_void, id: u64, time: Rational) -> u64;
	fn quercus_track_length(project: *const c_void, id: u64) -> Rational;
	fn quercus_track_list(project: *const c_void, id: u64) -> u64;
	fn quercus_track_height_pixels(project: *const c_void, id: u64) -> f64;
	fn quercus_track_internal_height_from_pixels(pixels: i32) -> f64;
	fn quercus_generator_create(
		project: *const c_void,
		type_id: *const u8,
		type_id_len: usize,
	) -> u64;
	fn quercus_project_unset_setting(
		project: *const c_void,
		key: *const u8,
		key_len: usize,
	) -> bool;
	fn quercus_project_graph_endpoints(
		project: *const c_void,
		input: *mut u64,
		output: *mut u64,
	) -> bool;
	fn quercus_track_index(project: *const c_void, id: u64) -> i32;

	fn quercus_block_in(project: *const c_void, id: u64) -> Rational;
	fn quercus_block_out(project: *const c_void, id: u64) -> Rational;
	fn quercus_block_length(project: *const c_void, id: u64) -> Rational;
	fn quercus_block_media_in(project: *const c_void, id: u64) -> Rational;
	fn quercus_block_media_out(project: *const c_void, id: u64) -> Rational;
	fn quercus_block_enabled(project: *const c_void, id: u64) -> bool;
	fn quercus_block_track(project: *const c_void, id: u64) -> u64;
	fn quercus_block_speed(project: *const c_void, id: u64) -> f64;
	fn quercus_block_link_count(project: *const c_void, id: u64) -> i32;
	fn quercus_block_link_at(project: *const c_void, id: u64, index: i32) -> u64;
	fn quercus_node_color(project: *const c_void, id: u64) -> i32;
	fn quercus_block_footage(project: *const c_void, id: u64) -> u64;
	fn quercus_block_multicam_source(project: *const c_void, id: u64) -> u64;
	fn quercus_block_transition_width(project: *const c_void, id: u64, start_edge: bool) -> Rational;
	fn quercus_project_node_count(project: *const c_void) -> i32;
	fn quercus_project_node_at(project: *const c_void, index: i32) -> u64;

	fn quercus_node_standard_value(
		project: *const c_void,
		id: u64,
		input_id: *const u8,
		input_id_len: usize,
		element: i32,
		out: *mut NodeValue,
	) -> bool;
	fn quercus_node_value_at_time(
		project: *const c_void,
		id: u64,
		input_id: *const u8,
		input_id_len: usize,
		element: i32,
		time: Rational,
		out: *mut NodeValue,
	) -> bool;

	fn quercus_undo_group_begin(name: *const u8, name_len: usize) -> bool;
	fn quercus_undo_group_end() -> bool;
	fn quercus_undo_group_abort() -> bool;
	fn quercus_undo_undo() -> bool;
	fn quercus_undo_redo() -> bool;

	fn quercus_footage_import(
		project: *const c_void,
		filename: *const u8,
		filename_len: usize,
	) -> u64;
	fn quercus_folder_create(
		project: *const c_void,
		parent: u64,
		name: *const u8,
		name_len: usize,
	) -> u64;
	fn quercus_node_remove(project: *const c_void, id: u64) -> bool;
	fn quercus_sequence_create(project: *const c_void, name: *const u8, name_len: usize) -> u64;
	fn quercus_track_add(project: *const c_void, sequence: u64, kind: i32) -> u64;
	fn quercus_block_create_clip(project: *const c_void) -> u64;
	fn quercus_block_create_gap(project: *const c_void) -> u64;
	fn quercus_block_create_transition(project: *const c_void) -> u64;
	fn quercus_block_create_adjustment(project: *const c_void) -> u64;
	fn quercus_track_append_block(project: *const c_void, track: u64, block: u64) -> bool;
	fn quercus_track_insert_block_after(
		project: *const c_void,
		track: u64,
		block: u64,
		before: u64,
	) -> bool;
	fn quercus_track_ripple_remove_block(project: *const c_void, track: u64, block: u64) -> bool;
	fn quercus_track_replace_block(project: *const c_void, track: u64, old: u64, new: u64) -> bool;
	fn quercus_block_set_length(project: *const c_void, block: u64, value: Rational) -> bool;
	fn quercus_block_set_length_and_media_in(
		project: *const c_void,
		block: u64,
		value: Rational,
	) -> bool;
	fn quercus_block_set_media_in(project: *const c_void, block: u64, value: Rational) -> bool;
	fn quercus_node_set_standard_value(
		project: *const c_void,
		id: u64,
		input_id: *const u8,
		input_id_len: usize,
		element: i32,
		value: NodeValue,
	) -> bool;
	fn quercus_node_connect(
		project: *const c_void,
		from: u64,
		to: u64,
		input_id: *const u8,
		input_id_len: usize,
		element: i32,
	) -> bool;
	fn quercus_node_disconnect(
		project: *const c_void,
		to: u64,
		input_id: *const u8,
		input_id_len: usize,
		element: i32,
	) -> bool;

	fn quercus_project_save(project: *const c_void, path: *const u8, path_len: usize) -> bool;
	fn quercus_project_load(path: *const u8, path_len: usize) -> *mut c_void;
	fn quercus_project_save_to_bytes(project: *const c_void, buf: *mut u8, cap: usize) -> i32;
	fn quercus_project_load_from_bytes(
		bytes: *const u8,
		len: usize,
		version_info: *mut i32,
		error: *mut i32,
	) -> *mut c_void;
	fn quercus_project_set_filename(project: *const c_void, path: *const u8, path_len: usize) -> bool;
	fn quercus_project_set_working_color_space(
		project: *const c_void,
		setting: *const u8,
		setting_len: usize,
	) -> bool;
	fn quercus_project_set_output_color_spec(
		project: *const c_void,
		gamut: *const u8,
		gamut_len: usize,
		transfer: *const u8,
		transfer_len: usize,
	) -> bool;
	fn quercus_project_deep_copy(project: *const c_void) -> *mut c_void;

	fn quercus_undo_can_undo() -> bool;
	fn quercus_undo_can_redo() -> bool;
	fn quercus_undo_count() -> i64;
	fn quercus_undo_index() -> i64;
	fn quercus_undo_jump(index: i64) -> bool;
	fn quercus_undo_clear() -> bool;
	fn quercus_undo_command_name(row: i64, buf: *mut u8, buf_len: usize) -> i32;
	fn quercus_undo_command_done(row: i64) -> i32;
	fn quercus_undo_add_observer(cb: Option<UndoObserverFn>, userdata: *mut c_void);
}

/// A node identity token (packed generational id). `INVALID` names no
/// node; ids are stable within a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(u64);

impl NodeId {
	/// The null token.
	pub const INVALID: NodeId = NodeId(u32::MAX as u64);

	/// Whether this token names a real node slot.
	pub fn is_valid(self) -> bool {
		unsafe { quercus_node_id_is_valid(self.0) }
	}

	/// Constructs a packed node token from its raw representation.
	/// This performs no graph lookup; call `is_valid` for untrusted IDs.
	pub fn from_raw(raw: u64) -> NodeId {
		NodeId(raw)
	}

	/// The raw token (crate-internal consumers: the render ABI).
	/// Returns the packed raw representation used by SDK DTOs and commands.
	pub fn raw(self) -> u64 {
		self.0
	}
}

/// Instantiates the generator node type `type_id` in `project`.
///
/// `type_id` is a factory type id such as
/// `org.olivevideoeditor.Olive.text`; it is looked up in the global
/// factory (built-ins plus discovered plugins) and must be registered
/// with the Generator category.
///
/// # Returns
///
/// The new node's token, or `None` when `type_id` is unknown, names a
/// non-generator type, or the project handle is invalid. The node is
/// created in the graph but not mounted in the bin tree.
pub fn create_generator(project: &Project, type_id: &str) -> Option<NodeId> {
	the_id(unsafe { quercus_generator_create(project.ptr, type_id.as_ptr(), type_id.len()) })
}

/// Converts a pixel track height to the graph's internal height units.
pub fn internal_height_from_pixels(pixels: i32) -> f64 {
	unsafe { quercus_track_internal_height_from_pixels(pixels) }
}

/// Converts a raw ABI token to `Some(id)` unless it is the engine's null
/// token.
fn the_id(raw: u64) -> Option<NodeId> {
	let id = NodeId::from_raw(raw);
	id.is_valid().then_some(id)
}

/// A node's coarse kind.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
	/// A project-bin folder.
	Folder = 1,
	/// A footage (media) node.
	Footage = 2,
	/// A sequence.
	Sequence = 3,
	/// A track.
	Track = 4,
	/// A clip block.
	Clip = 5,
	/// A gap block.
	Gap = 6,
	/// A transition block.
	Transition = 7,
	/// An adjustment layer.
	Adjustment = 8,
	/// A track list.
	TrackList = 9,
}

impl NodeKind {
	/// Converts a C ABI node-kind discriminant into a [`NodeKind`].
	///
	/// Returns `None` for `0` (no kind) or any value outside `1..=9`. This
	/// mapping mirrors the `#[repr(i32)]` discriminants on [`NodeKind`] and
	/// must stay in sync with `include/quercus_engine.h`.
	fn from_i32(v: i32) -> Option<NodeKind> {
		match v {
			1 => Some(NodeKind::Folder),
			2 => Some(NodeKind::Footage),
			3 => Some(NodeKind::Sequence),
			4 => Some(NodeKind::Track),
			5 => Some(NodeKind::Clip),
			6 => Some(NodeKind::Gap),
			7 => Some(NodeKind::Transition),
			8 => Some(NodeKind::Adjustment),
			9 => Some(NodeKind::TrackList),
			_ => None,
		}
	}
}

/// A video stream's parameters as carried by a footage node.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StreamVideoParams {
	/// The `width` field.
	pub width: i32,
	/// The `height` field.
	pub height: i32,
	/// The `frame_rate` field.
	pub frame_rate: Rational,
	/// The `pixel_format` field.
	pub pixel_format: i32,
	/// The `channels` field.
	pub channels: i32,
	/// The `interlaced` field.
	pub interlaced: i32,
}

/// An audio stream's parameters as carried by a footage node.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StreamAudioParams {
	/// The `sample_rate` field.
	pub sample_rate: i32,
	/// The `channel_layout` field.
	pub channel_layout: u64,
	/// The `format` field.
	pub format: i32,
}

/// A node parameter value (POD; layout matches the engine's
/// `QuercusNodeValue`). String-typed values are not bridged.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct NodeValue {
	/// `NodeValueKind` discriminant (0 = none … 9 = string).
	pub kind: i32,
	/// INT/COMBO value, BOOL 0/1, RATIONAL numerator.
	pub num: i64,
	/// RATIONAL denominator.
	pub den: i64,
	/// FLOAT `f[0]`; VEC2/3/4 `f[0..n-1]`; COLOR r,g,b,a.
	pub f: [f64; 4],
}

impl NodeValue {
	/// The null value.
	pub fn none() -> Self {
		Self::default()
	}

	/// Builds a `FLOAT` value (`kind = 2`) with `value` in the first float
	/// slot; `num` and `den` stay zero.
	///
	/// Pairs with [`Project::set_standard_value`]. Use [`NodeValue::rational`]
	/// for exact time values and [`NodeValue::int`] for integral parameters.
	pub fn float(value: f64) -> Self {
		let mut raw = Self {
			kind: 2,
			..Self::default()
		};
		raw.f[0] = value;
		raw
	}

	/// Builds an `INT` value (`kind = 1`) holding `value` in `num` with
	/// `den = 1`; the float slots stay zero.
	///
	/// Use for integer and combo inputs; [`NodeValue::float`] covers
	/// continuous float parameters.
	pub fn int(value: i64) -> Self {
		Self {
			kind: 1,
			num: value,
			den: 1,
			..Self::default()
		}
	}

	/// Builds a `BOOL` value (`kind = 3`): `num` is `0`/`1`, `den` is `1`
	/// and the float slots stay zero.
	///
	/// The engine converts it to the input's declared boolean type; see
	/// [`Project::set_standard_value`].
	pub fn bool(value: bool) -> Self {
		Self {
			kind: 3,
			num: i64::from(value),
			den: 1,
			..Self::default()
		}
	}

	/// Builds a `RATIONAL` value (`kind = 4`) from an exact rational-seconds
	/// number, copying `value.num` and `value.den` into the value.
	///
	/// `den` must be non-zero for a usable time (`0/0` is the engine's null
	/// rational); the float slots stay zero. Use [`NodeValue::float`] for
	/// plain numeric parameters.
	pub fn rational(value: Rational) -> Self {
		Self {
			kind: 4,
			num: value.num,
			den: value.den,
			..Self::default()
		}
	}
}

/// An open project. `Clone` adds a reference, `Drop` releases.
pub struct Project {
	ptr: *mut c_void,
}

unsafe impl Send for Project {}
unsafe impl Sync for Project {}

impl Project {
	/// Converts this owned handle into the storage migration representation used
	/// by legacy hosts. The same live Arc is retained; this handle is consumed.
	pub fn into_storage_project_handle(self) -> crate::editor::StorageProjectHandle {
		let handle = unsafe { quercus_project_into_storage_handle(self.ptr) };
		std::mem::forget(self);
		handle
	}
	/// A new, initialized project (empty graph with a root folder).
	pub fn new() -> Self {
		Self {
			ptr: unsafe { quercus_project_new() },
		}
	}

	/// Wraps a raw engine handle (crate-internal: the storage ABI hands
	/// projects back this way). The pointer takes over one reference.
	pub(crate) fn from_raw(ptr: *mut c_void) -> Self {
		Self { ptr }
	}

	/// Borrows the raw engine handle (crate-internal: storage/render
	/// calls take it as an argument).
	pub(crate) fn as_raw(&self) -> *mut c_void {
		self.ptr
	}

	/// Transfers the raw reference to another SDK handle without dropping it.
	pub(crate) fn into_raw(self) -> *mut c_void {
		let ptr = self.ptr;
		std::mem::forget(self);
		ptr
	}

	/// The project display name.
	pub fn name(&self) -> String {
		string_out(|buf, len| unsafe { quercus_project_name(self.ptr, buf, len) }).unwrap_or_default()
	}

	/// The project file path ("" when unsaved).
	pub fn filename(&self) -> String {
		string_out(|buf, len| unsafe { quercus_project_filename(self.ptr, buf, len) })
			.unwrap_or_default()
	}

	/// The display-friendly file path.
	pub fn pretty_filename(&self) -> String {
		string_out(|buf, len| unsafe { quercus_project_pretty_filename(self.ptr, buf, len) })
			.unwrap_or_default()
	}

	/// Whether the project has unsaved changes.
	pub fn modified(&self) -> bool {
		unsafe { quercus_project_modified(self.ptr) }
	}

	/// The stable uuid used by the project library.
	pub fn uuid(&self) -> String {
		string_out(|buf, len| unsafe { quercus_project_uuid(self.ptr, buf, len) }).unwrap_or_default()
	}

	/// Reads the persisted project setting `key` from the project file's
	/// settings map.
	///
	/// # Returns
	///
	/// The stored string, or `None` when the key is absent or the project
	/// handle is invalid. An empty stored value is returned as `Some("")`.
	/// Pairs with [`Project::set_setting`] and
	/// [`Project::unset_setting`].
	pub fn setting(&self, key: &str) -> Option<String> {
		string_out(|buf, len| unsafe {
			quercus_project_setting(self.ptr, key.as_ptr(), key.len(), buf, len)
		})
	}

	/// Writes the persisted project setting `key` to `value`, replacing any
	/// previous value, and marks the project modified.
	///
	/// Settings live in the project file's settings map and are not undoable.
	/// `key` must be non-empty. Returns `false` when the key is empty or the
	/// project handle is invalid.
	pub fn set_setting(&self, key: &str, value: &str) -> bool {
		unsafe {
			quercus_project_set_setting(
				self.ptr,
				key.as_ptr(),
				key.len(),
				value.as_ptr(),
				value.len(),
			)
		}
	}

	/// `(cache mode, custom path)`; mode 2 uses the custom path.
	pub fn cache_location(&self) -> (i32, String) {
		let mode = std::cell::Cell::new(0);
		let path = string_out(|buf, len| unsafe {
			quercus_project_cache_location(self.ptr, mode.as_ptr(), buf, len)
		})
		.unwrap_or_default();
		(mode.get(), path)
	}

	/// Writes the project's cache policy and custom cache path, marking the
	/// project modified.
	///
	/// `mode` is clamped to `0..=2`; mode `2` means "custom location" and
	/// stores `path` (trimmed), while the other modes store an empty path and
	/// use the default location. Returns `false` when the project handle is
	/// invalid. Read back with [`Project::cache_location`].
	pub fn set_cache_location(&self, mode: i32, path: &str) -> bool {
		unsafe {
			quercus_project_set_cache_location(self.ptr, mode, path.as_ptr(), path.len())
		}
	}

	/// The root folder of the bin tree.
	pub fn root(&self) -> NodeId {
		NodeId::from_raw(unsafe { quercus_project_root(self.ptr) })
	}

	// -- generic node queries ------------------------------------------

	/// Returns the node's coarse [`NodeKind`] (folder, footage, sequence,
	/// track, clip, gap, transition, adjustment or track list).
	///
	/// Returns `None` for a stale token or an engine kind this build does
	/// not know. Pairs with [`Project::node_type_id`] for the exact type.
	pub fn node_kind(&self, id: NodeId) -> Option<NodeKind> {
		NodeKind::from_i32(unsafe { quercus_node_kind(self.ptr, id.0) })
	}

	/// Returns the node's full factory type id string (for example
	/// `org.olivevideoeditor.Olive.video`), or `None` when the token names
	/// no live node.
	///
	/// This is the exact type; [`Project::node_kind`] gives the coarse
	/// category.
	pub fn node_type_id(&self, id: NodeId) -> Option<String> {
		string_out(|buf, len| unsafe { quercus_node_type_id(self.ptr, id.0, buf, len) })
	}

	// -- folder --------------------------------------------------------

	/// Returns the number of direct children of the bin folder `folder`.
	///
	/// Children are the footage, sequence and folder nodes mounted under that
	/// folder (not recursive). `0` when `folder` is not a folder or the token
	/// is stale. Pairs with [`Project::folder_child_at`].
	pub fn folder_child_count(&self, folder: NodeId) -> usize {
		unsafe { quercus_folder_child_count(self.ptr, folder.0) as usize }
	}

	/// The `index`-th child of a folder (`NodeId::INVALID` when out of
	/// range).
	pub fn folder_child_at(&self, folder: NodeId, index: usize) -> NodeId {
		NodeId::from_raw(unsafe { quercus_folder_child_at(self.ptr, folder.0, index as i32) })
	}

	// -- footage -------------------------------------------------------

	/// Returns the footage's absolute source file path, or `None` when
	/// `footage` is not a footage node. An unset path is returned as
	/// `Some("")`.
	pub fn footage_filename(&self, footage: NodeId) -> Option<String> {
		string_out(|buf, len| unsafe { quercus_footage_filename(self.ptr, footage.0, buf, len) })
	}

	/// Returns the total number of streams (video + audio + subtitle) in the
	/// footage's probed media.
	///
	/// `0` for a non-footage token or a footage that has not been probed yet.
	/// Pairs with the per-kind counts.
	pub fn footage_total_stream_count(&self, footage: NodeId) -> usize {
		unsafe { quercus_footage_total_stream_count(self.ptr, footage.0) as usize }
	}

	/// Returns the number of video streams in the footage's probed media;
	/// `0` for a non-footage token or an unprobed file. Stream ordinals for
	/// [`Project::footage_video_params`] count video streams only.
	pub fn footage_video_stream_count(&self, footage: NodeId) -> usize {
		unsafe { quercus_footage_video_stream_count(self.ptr, footage.0) as usize }
	}

	/// Returns the number of audio streams in the footage's probed media;
	/// `0` for a non-footage token or an unprobed file. Stream ordinals for
	/// [`Project::footage_audio_params`] count audio streams only.
	pub fn footage_audio_stream_count(&self, footage: NodeId) -> usize {
		unsafe { quercus_footage_audio_stream_count(self.ptr, footage.0) as usize }
	}

	/// Returns the number of subtitle streams in the footage's probed media;
	/// `0` for a non-footage token, an unprobed file, or media without
	/// subtitle tracks.
	pub fn footage_subtitle_stream_count(&self, footage: NodeId) -> usize {
		unsafe { quercus_footage_subtitle_stream_count(self.ptr, footage.0) as usize }
	}

	/// Returns the footage's total duration in rational seconds: the longest
	/// stream duration reported by the probe.
	///
	/// Returns the null rational (`den == 0`) when the token is not footage;
	/// an unprobed footage reports `0/1`. Pairs with
	/// [`Project::footage_video_length`].
	pub fn footage_duration(&self, footage: NodeId) -> Rational {
		unsafe { quercus_footage_duration(self.ptr, footage.0) }
	}

	/// Returns the duration of the footage's longest video stream in rational
	/// seconds, or the null rational (`den == 0`) when the token is not
	/// footage.
	///
	/// Use [`Project::footage_duration`] for the overall media length
	/// including audio.
	pub fn footage_video_length(&self, footage: NodeId) -> Rational {
		unsafe { quercus_footage_video_length(self.ptr, footage.0) }
	}

	/// Returns the parameters of the `ordinal`-th video stream of `footage`
	/// (zero-based; counts video streams only).
	///
	/// The result carries width, height, frame rate, pixel format, channel
	/// count and the interlaced flag as reported by the probe.
	///
	/// # Returns
	///
	/// `None` when `footage` is not footage or `ordinal` is out of range;
	/// check [`Project::footage_video_stream_count`] first.
	pub fn footage_video_params(&self, footage: NodeId, ordinal: usize) -> Option<StreamVideoParams> {
		let mut out = StreamVideoParams::default();
		if unsafe { quercus_footage_video_params(self.ptr, footage.0, ordinal as i32, &mut out) } {
			Some(out)
		} else {
			None
		}
	}

	/// Returns the parameters of the `ordinal`-th audio stream of `footage`
	/// (zero-based; counts audio streams only).
	///
	/// The result carries sample rate, channel layout and sample format as
	/// reported by the probe.
	///
	/// # Returns
	///
	/// `None` when `footage` is not footage or `ordinal` is out of range;
	/// check [`Project::footage_audio_stream_count`] first.
	pub fn footage_audio_params(&self, footage: NodeId, ordinal: usize) -> Option<StreamAudioParams> {
		let mut out = StreamAudioParams::default();
		if unsafe { quercus_footage_audio_params(self.ptr, footage.0, ordinal as i32, &mut out) } {
			Some(out)
		} else {
			None
		}
	}

	/// Returns the per-footage proxy override, or `None` when global defaults apply.
	pub fn footage_custom_proxy_params(&self, footage: NodeId) -> Option<crate::codec::ProxyParams> {
		let mut out = crate::codec::ProxyParams::default();
		unsafe { quercus_footage_custom_proxy_params(self.ptr, footage.0, &mut out) }.then_some(out)
	}

	/// Returns custom proxy parameters when present, otherwise global defaults.
	pub fn footage_effective_proxy_params(&self, footage: NodeId) -> Option<crate::codec::ProxyParams> {
		let mut out = crate::codec::ProxyParams::default();
		unsafe { quercus_footage_effective_proxy_params(self.ptr, footage.0, &mut out) }.then_some(out)
	}

	/// Returns source-start timecode only when the footage carries it.
	pub fn footage_source_start_time(&self, footage: NodeId) -> Option<Rational> {
		let mut out = Rational::NULL;
		unsafe { quercus_footage_source_start_time(self.ptr, footage.0, &mut out) }.then_some(out)
	}

	// -- sequence / track list / track ---------------------------------

	/// Returns the number of track lists in `sequence`; `0` for a
	/// non-sequence token.
	///
	/// Track lists are ordered video first, then audio, then subtitle; use
	/// [`Project::sequence_track_list_at`] with an index below this count.
	pub fn sequence_track_list_count(&self, sequence: NodeId) -> usize {
		unsafe { quercus_sequence_track_list_count(self.ptr, sequence.0) as usize }
	}

	/// Returns the token of the `index`-th track list of `sequence` (video
	/// lists first, then audio, then subtitle).
	///
	/// Returns [`NodeId::INVALID`] when `index` is out of range or
	/// `sequence` is not a sequence.
	pub fn sequence_track_list_at(&self, sequence: NodeId, index: usize) -> NodeId {
		NodeId::from_raw(unsafe { quercus_sequence_track_list_at(self.ptr, sequence.0, index as i32) })
	}

	/// Returns the sequence playhead position in rational seconds, relative
	/// to the sequence start.
	///
	/// Returns the null rational (`den == 0`) for a non-sequence token.
	pub fn sequence_playhead(&self, sequence: NodeId) -> Rational {
		unsafe { quercus_sequence_playhead(self.ptr, sequence.0) }
	}

	/// Returns the sequence's length in rational seconds.
	///
	/// The length is the extent of the content on its tracks, falling back to
	/// the last length requested when the content is empty, so an empty
	/// sequence can still report its configured duration.
	///
	/// Returns the null rational (`den == 0`) for a non-sequence token.
	pub fn sequence_length(&self, sequence: NodeId) -> Rational {
		unsafe { quercus_sequence_length(self.ptr, sequence.0) }
	}

	/// The sequence's frame rate (the null rational when it has no
	/// video params).
	pub fn sequence_frame_rate(&self, sequence: NodeId) -> Rational {
		unsafe { quercus_sequence_frame_rate(self.ptr, sequence.0) }
	}

	/// The sequence's output geometry (width, height); None when it has
	/// no video params.
	pub fn sequence_geometry(&self, sequence: NodeId) -> Option<(i32, i32)> {
		let (mut w, mut h) = (0, 0);
		unsafe { quercus_sequence_geometry(self.ptr, sequence.0, &mut w, &mut h) }.then_some((w, h))
	}

	/// Sets the sequence's interlaced flag (true when it was a sequence).
	pub fn sequence_set_interlaced(&self, sequence: NodeId, interlaced: bool) -> bool {
		unsafe { quercus_sequence_set_interlaced(self.ptr, sequence.0, interlaced) }
	}

	/// Whether the sequence uses interlaced video (`None` for a stale id).
	pub fn sequence_interlaced(&self, sequence: NodeId) -> Option<bool> {
		match unsafe { quercus_sequence_interlaced(self.ptr, sequence.0) } {
			0 => Some(false),
			1 => Some(true),
			_ => None,
		}
	}

	/// Sets the sequence's output video params (width/height/frame
	/// rate; pixel format stays F32 RGBA).
	pub fn sequence_set_video_params(
		&self,
		sequence: NodeId,
		width: i32,
		height: i32,
		frame_rate_num: i32,
		frame_rate_den: i32,
	) -> bool {
		unsafe {
			quercus_sequence_set_video_params(
				self.ptr,
				sequence.0,
				width,
				height,
				frame_rate_num,
				frame_rate_den,
			)
		}
	}

	/// Returns the node's user-visible label (set on import/creation or via
	/// [`Project::node_set_label`]).
	///
	/// Returns `""` when the label is unset or the token is unknown; the
	/// label is not unique and may be empty for generated nodes.
	pub fn node_label(&self, id: NodeId) -> String {
		string_out(|buf, len| unsafe { quercus_node_label(self.ptr, id.0, buf, len) })
			.unwrap_or_default()
	}

	/// Sets the node's user label (a direct write, not an undoable
	/// edit).
	pub fn node_set_label(&self, id: NodeId, label: &str) -> bool {
		unsafe { quercus_node_set_label(self.ptr, id.0, label.as_ptr(), label.len()) }
	}

	/// Returns the track list's media type as a `TrackType` code:
	/// `0` = video, `1` = audio, `2` = subtitle.
	///
	/// Returns `-1` when `tracklist` is not a track list or the token is
	/// stale. Pairs with [`Project::track_kind`].
	pub fn tracklist_kind(&self, tracklist: NodeId) -> i32 {
		unsafe { quercus_tracklist_kind(self.ptr, tracklist.0) }
	}

	/// Returns the number of tracks in `tracklist`; `0` for a
	/// non-track-list token. Pairs with [`Project::tracklist_track_at`].
	pub fn tracklist_track_count(&self, tracklist: NodeId) -> usize {
		unsafe { quercus_tracklist_track_count(self.ptr, tracklist.0) as usize }
	}

	/// Returns the token of the `index`-th track of `tracklist` in stack
	/// order (`0` = bottom of the stack).
	///
	/// Returns [`NodeId::INVALID`] when `index` is out of range or
	/// `tracklist` is not a track list.
	pub fn tracklist_track_at(&self, tracklist: NodeId, index: usize) -> NodeId {
		NodeId::from_raw(unsafe { quercus_tracklist_track_at(self.ptr, tracklist.0, index as i32) })
	}

	/// Returns the track's media type as a `TrackType` code:
	/// `0` = video, `1` = audio, `2` = subtitle.
	///
	/// Returns `-1` when `track` is not a track. Pairs with
	/// [`Project::tracklist_kind`].
	pub fn track_kind(&self, track: NodeId) -> i32 {
		unsafe { quercus_track_kind(self.ptr, track.0) }
	}

	/// Returns the track's height in the graph's internal height units (not
	/// pixels) as stored on the track.
	///
	/// Use [`Project::track_height_pixels`] for the display height and
	/// [`internal_height_from_pixels`] to convert an edited pixel height
	/// before writing it back. Returns `0.0` for a non-track token.
	pub fn track_height(&self, track: NodeId) -> f64 {
		unsafe { quercus_track_height(self.ptr, track.0) }
	}

	/// Whether the track's muted flag is set: silenced on audio tracks,
	/// hidden on video/subtitle tracks (one flag drives both).
	///
	/// Returns `false` for a non-track token. Pairs with
	/// [`Project::track_locked`].
	pub fn track_muted(&self, track: NodeId) -> bool {
		unsafe { quercus_track_muted(self.ptr, track.0) }
	}

	/// Whether the track's locked flag is set. Editing commands that move or
	/// remove blocks skip locked tracks.
	///
	/// Returns `false` for a non-track token. This is a presentation flag:
	/// low-level mutation methods such as [`Project::track_append_block`]
	/// do not enforce it.
	pub fn track_locked(&self, track: NodeId) -> bool {
		unsafe { quercus_track_locked(self.ptr, track.0) }
	}

	/// Returns the number of blocks on `track` (clips, gaps, transitions
	/// and adjustments all count); `0` for a non-track token.
	///
	/// Blocks are stored in timeline order; [`Project::track_block_at`]
	/// indexes them.
	pub fn track_block_count(&self, track: NodeId) -> usize {
		unsafe { quercus_track_block_count(self.ptr, track.0) as usize }
	}

	/// Returns the token of the `index`-th block on `track` in timeline order
	/// (`0` = the first/leftmost block).
	///
	/// Returns [`NodeId::INVALID`] when `index` is out of range or the token
	/// is not a track.
	pub fn track_block_at(&self, track: NodeId, index: usize) -> NodeId {
		NodeId::from_raw(unsafe { quercus_track_block_at(self.ptr, track.0, index as i32) })
	}

	/// Returns the block on `track` whose half-open range `[in, out)`
	/// contains `time`, or [`NodeId::INVALID`] when `time` falls in a gap, on
	/// an out-point boundary, or the token is not a track.
	///
	/// `time` is in rational seconds on the sequence timeline. Pairs with
	/// [`Project::track_block_at`] for ordered traversal.
	pub fn track_block_containing_time(&self, track: NodeId, time: Rational) -> NodeId {
		NodeId::from_raw(unsafe { quercus_track_block_containing_time(self.ptr, track.0, time) })
	}

	/// Returns the track's total length in rational seconds: the out-point of
	/// its last block.
	///
	/// Returns the null rational (`den == 0`) when `track` is not a track;
	/// an empty track reports `0/1`.
	pub fn track_length(&self, track: NodeId) -> Rational {
		unsafe { quercus_track_length(self.ptr, track.0) }
	}

	// -- block ----------------------------------------------------------

	/// Returns the block's timeline in-point in rational seconds: where the
	/// block starts on the sequence timeline.
	///
	/// Returns the null rational (`den == 0`) for a non-block token.
	pub fn block_in(&self, block: NodeId) -> Rational {
		unsafe { quercus_block_in(self.ptr, block.0) }
	}

	/// Returns the block's timeline out-point in rational seconds (the
	/// exclusive end of the block's timeline range).
	///
	/// Returns the null rational (`den == 0`) for a non-block token.
	pub fn block_out(&self, block: NodeId) -> Rational {
		unsafe { quercus_block_out(self.ptr, block.0) }
	}

	/// Returns the block's timeline length in rational seconds, equal to
	/// `block_out - block_in` for a normal block.
	///
	/// Returns the null rational (`den == 0`) for a non-block token.
	pub fn block_length(&self, block: NodeId) -> Rational {
		unsafe { quercus_block_length(self.ptr, block.0) }
	}

	/// Returns the block's media in-point in rational seconds: the position
	/// in the source media where playback starts.
	///
	/// The media in-point differs from the timeline in-point by the left-edge
	/// trim. Returns the null rational (`den == 0`) for a non-block token.
	pub fn block_media_in(&self, block: NodeId) -> Rational {
		unsafe { quercus_block_media_in(self.ptr, block.0) }
	}

	/// Returns the block's media out-point in rational seconds: the source
	/// position where playback ends.
	///
	/// Returns the null rational (`den == 0`) for a non-block token. The
	/// media range [`Project::block_media_in`] up to this point is what the
	/// block plays.
	pub fn block_media_out(&self, block: NodeId) -> Rational {
		unsafe { quercus_block_media_out(self.ptr, block.0) }
	}

	/// Whether the block is enabled; disabled clips, gaps, transitions and
	/// adjustments contribute nothing to playback or render.
	///
	/// Returns `false` for a non-block or stale token (the flag lives on
	/// every block kind).
	pub fn block_enabled(&self, block: NodeId) -> bool {
		unsafe { quercus_block_enabled(self.ptr, block.0) }
	}

	/// The track's display height in pixels (24px floor, 64px default).
	pub fn track_height_pixels(&self, track: NodeId) -> f64 {
		unsafe { quercus_track_height_pixels(self.ptr, track.0) }
	}

	/// Returns the track list that owns `track`, or `None` when the track is
	/// detached, stale, or not a track.
	///
	/// Pairs with [`Project::tracklist_track_at`].
	pub fn track_list(&self, track: NodeId) -> Option<NodeId> {
		the_id(unsafe { quercus_track_list(self.ptr, track.0) })
	}

	/// Returns the zero-based index of `track` in its owning track list's
	/// stack order (`0` = bottom of the stack).
	///
	/// Returns `-1` when `track` is not a track or has no owning list.
	pub fn track_index(&self, track: NodeId) -> i32 {
		unsafe { quercus_track_index(self.ptr, track.0) }
	}

	/// Returns the track that currently holds `block`, or `None` when the
	/// block is not placed on a track, is stale, or is not a block.
	///
	/// Pairs with [`Project::track_block_at`] and
	/// [`Project::track_append_block`].
	pub fn block_track(&self, block: NodeId) -> Option<NodeId> {
		the_id(unsafe { quercus_block_track(self.ptr, block.0) })
	}

	/// Returns the block's playback speed multiplier: `1.0` = normal,
	/// `0.5` = half speed, `2.0` = double speed.
	///
	/// Returns `1.0` for a stale or non-block token. Speed changes the
	/// timeline-to-media time mapping, not the block's timeline length.
	pub fn block_speed(&self, block: NodeId) -> f64 {
		unsafe { quercus_block_speed(self.ptr, block.0) }
	}

	/// Returns the linked-block tokens recorded for `block` (its A/V link
	/// group), in stored order; stale partner tokens are filtered out.
	///
	/// The list is empty for an unlinked or non-block token.
	/// [`Project::block_is_linked`] is the cheap predicate.
	pub fn block_links(&self, block: NodeId) -> Vec<NodeId> {
		let count = unsafe { quercus_block_link_count(self.ptr, block.0) }.max(0);
		(0..count)
			.filter_map(|index| the_id(unsafe { quercus_block_link_at(self.ptr, block.0, index) }))
			.collect()
	}

	/// Whether `block` belongs to an A/V link group, i.e. whether
	/// [`Project::block_links`] reports at least one partner.
	pub fn block_is_linked(&self, block: NodeId) -> bool {
		!self.block_links(block).is_empty()
	}

	/// Returns the node's color override: a palette index in `0..=7` chosen
	/// when a clip is created and kept for the node's lifetime, or `-1` when
	/// no override is set / the token is stale.
	pub fn node_color(&self, id: NodeId) -> i32 {
		unsafe { quercus_node_color(self.ptr, id.0) }
	}

	/// Returns the footage node that supplies `block`'s media, or `None` when
	/// `block` is not a clip, is stale, or has no footage connected yet.
	pub fn block_footage(&self, block: NodeId) -> Option<NodeId> {
		the_id(unsafe { quercus_block_footage(self.ptr, block.0) })
	}

	/// The multicam source the block reads from, when its texture input is
	/// a multicam node.
	pub fn block_multicam_source(&self, block: NodeId) -> Option<NodeId> {
		the_id(unsafe { quercus_block_multicam_source(self.ptr, block.0) })
	}

	/// The width of the transition on one edge of the clip, if any.
	pub fn block_transition_width(&self, clip: NodeId, start_edge: bool) -> Option<Rational> {
		let width = unsafe { quercus_block_transition_width(self.ptr, clip.0, start_edge) };
		(width.den != 0).then_some(width)
	}

	/// Removes the persisted project setting `key`, marking the project
	/// modified when a value was present.
	///
	/// Returns `false` when the key is empty, absent, or the project handle
	/// is invalid. Not undoable; pairs with [`Project::set_setting`].
	pub fn unset_setting(&self, key: &str) -> bool {
		unsafe { quercus_project_unset_setting(self.ptr, key.as_ptr(), key.len()) }
	}

	/// The graph's virtual input/output endpoints, when present.
	pub fn graph_endpoints(&self) -> Option<(NodeId, NodeId)> {
		let (mut input, mut output) = (0u64, 0u64);
		unsafe { quercus_project_graph_endpoints(self.ptr, &mut input, &mut output) }
			.then(|| (NodeId::from_raw(input), NodeId::from_raw(output)))
	}

	/// Number of live nodes in the graph.
	pub fn node_count(&self) -> usize {
		unsafe { quercus_project_node_count(self.ptr) }.max(0) as usize
	}

	/// Returns the token of the `index`-th live node in graph arena order,
	/// or `None` when `index` is past [`Project::node_count`].
	///
	/// Arena order is an implementation detail and changes as nodes are
	/// added/removed; use it to iterate the graph (as [`Project::node_ids`]
	/// does), not as a stable node order.
	pub fn node_at(&self, index: usize) -> Option<NodeId> {
		the_id(unsafe { quercus_project_node_at(self.ptr, index as i32) })
	}

	/// Every live node in arena order.
	pub fn node_ids(&self) -> Vec<NodeId> {
		(0..self.node_count()).filter_map(|i| self.node_at(i)).collect()
	}

	// -- node parameters -------------------------------------------------

	/// Reads the static (non-animated) value of `id`'s input `input_id` at
	/// element index `element` (`-1` = the scalar input).
	///
	/// Returns `None` for an unknown node/input, an out-of-range element, or
	/// a declared value type without a POD representation (string-typed
	/// inputs are not bridged yet). Use [`Project::value_at_time`] for the
	/// keyframe-interpolated value.
	pub fn standard_value(&self, id: NodeId, input_id: &str, element: i32) -> Option<NodeValue> {
		let mut out = NodeValue::default();
		if unsafe {
			quercus_node_standard_value(self.ptr, id.0, input_id.as_ptr(), input_id.len(), element, &mut out)
		} {
			Some(out)
		} else {
			None
		}
	}

	/// The input's value at `time` (keyframe-interpolated when
	/// animated).
	pub fn value_at_time(
		&self,
		id: NodeId,
		input_id: &str,
		element: i32,
		time: Rational,
	) -> Option<NodeValue> {
		let mut out = NodeValue::default();
		if unsafe {
			quercus_node_value_at_time(
				self.ptr,
				id.0,
				input_id.as_ptr(),
				input_id.len(),
				element,
				time,
				&mut out,
			)
		} {
			Some(out)
		} else {
			None
		}
	}
	// ---------------------------------------------------------------------
	// Editing (undo-integrated: every mutation pushes a command; batch
	// several calls into one undo step with `undo_group_begin`/`end`)
	// ---------------------------------------------------------------------

	/// Imports a media file into the bin (probes first; a failed probe
	/// leaves nothing behind). Undoable.
	pub fn import_footage(&self, filename: &str) -> Option<NodeId> {
		let id = unsafe { quercus_footage_import(self.ptr, filename.as_ptr(), filename.len()) };
		let id = NodeId::from_raw(id);
		id.is_valid().then_some(id)
	}

	/// Creates a new empty bin folder named `name` under the folder
	/// `parent` (pass [`Project::root`] to add at the top level). Undoable.
	///
	/// `parent` must be a well-formed folder token; `name` becomes both the
	/// folder's display name and the node label, and may be empty.
	///
	/// # Returns
	///
	/// The new folder's token, or `None` when the parent token is malformed
	/// or the undo push fails. A failed push can leave an unparented folder
	/// in the graph (the node is created before the insertion command).
	pub fn create_folder(&self, parent: NodeId, name: &str) -> Option<NodeId> {
		let id = unsafe { quercus_folder_create(self.ptr, parent.0, name.as_ptr(), name.len()) };
		let id = NodeId::from_raw(id);
		id.is_valid().then_some(id)
	}

	/// Deletes `id` from the project graph entirely, along with the edges
	/// that referenced it. Undoable.
	///
	/// Use the type-specific removal helpers where one exists (for example
	/// [`Project::track_ripple_remove_block`]) so track membership and
	/// timeline layout stay consistent. Returns `false` when the project
	/// handle or the token is invalid.
	pub fn remove_node(&self, id: NodeId) -> bool {
		unsafe { quercus_node_remove(self.ptr, id.0) }
	}

	/// Creates a sequence with the default 2V+2A track layout, mounted in
	/// the bin root. (Creation itself is not undoable, like the app.)
	pub fn create_sequence(&self, name: &str) -> Option<NodeId> {
		let id = unsafe { quercus_sequence_create(self.ptr, name.as_ptr(), name.len()) };
		let id = NodeId::from_raw(id);
		id.is_valid().then_some(id)
	}

	/// Adds a track to the sequence's list of the given media kind
	/// (`TrackType` code; 0 = video, 1 = audio, 2 = subtitle). Undoable.
	pub fn add_track(&self, sequence: NodeId, kind: i32) -> Option<NodeId> {
		let id = unsafe { quercus_track_add(self.ptr, sequence.0, kind) };
		let id = NodeId::from_raw(id);
		id.is_valid().then_some(id)
	}

	/// Creates a clip block (not on any track yet — placement is the
	/// undoable step).
	pub fn create_clip(&self) -> Option<NodeId> {
		let id = NodeId::from_raw(unsafe { quercus_block_create_clip(self.ptr) });
		id.is_valid().then_some(id)
	}

	/// Creates a gap block.
	pub fn create_gap(&self) -> Option<NodeId> {
		let id = NodeId::from_raw(unsafe { quercus_block_create_gap(self.ptr) });
		id.is_valid().then_some(id)
	}

	/// Creates a transition block.
	pub fn create_transition(&self) -> Option<NodeId> {
		let id = NodeId::from_raw(unsafe { quercus_block_create_transition(self.ptr) });
		id.is_valid().then_some(id)
	}

	/// Creates an adjustment block.
	pub fn create_adjustment(&self) -> Option<NodeId> {
		let id = NodeId::from_raw(unsafe { quercus_block_create_adjustment(self.ptr) });
		id.is_valid().then_some(id)
	}

	/// Places `block` on `track` and records `track` as the block's owning
	/// track. Undoable.
	///
	/// This is the append helper: it delegates to
	/// [`Project::track_insert_block_after`] with no predecessor, which the
	/// insert command treats as a front insert. `block` must not already be
	/// on a track. Returns `false` when the project handle or either token
	/// is invalid.
	pub fn track_append_block(&self, track: NodeId, block: NodeId) -> bool {
		unsafe { quercus_track_append_block(self.ptr, track.0, block.0) }
	}

	/// Inserts `block` into `track` immediately after `before`, or at the
	/// front when `before` is `None` (the insert command treats a missing
	/// predecessor as a prepend). Undoable.
	///
	/// `before` must be a block already on `track`; `block` must not be on a
	/// track yet. Returns `false` when the project handle, `track` or
	/// `block` is invalid.
	pub fn track_insert_block_after(
		&self,
		track: NodeId,
		block: NodeId,
		before: Option<NodeId>,
	) -> bool {
		let before = before.unwrap_or(NodeId::INVALID);
		unsafe { quercus_track_insert_block_after(self.ptr, track.0, block.0, before.0) }
	}

	/// Removes `block` from `track`, closing the gap (ripple). Undoable.
	pub fn track_ripple_remove_block(&self, track: NodeId, block: NodeId) -> bool {
		unsafe { quercus_track_ripple_remove_block(self.ptr, track.0, block.0) }
	}

	/// Replaces the block `old` with `new` at `old`'s position on `track`,
	/// moving the track ownership to `new`. Undoable.
	///
	/// Both tokens must be live blocks. The engine's `Track::replace_block`
	/// documents that the two blocks must have equal lengths; neither that
	/// method nor this wrapper enforces it.
	///
	/// Returns `false` when the project handle or any token is invalid.
	pub fn track_replace_block(&self, track: NodeId, old: NodeId, new: NodeId) -> bool {
		unsafe { quercus_track_replace_block(self.ptr, track.0, old.0, new.0) }
	}

	/// Resizes the block from its right edge to `length`, leaving the
	/// timeline in-point and the media in-point fixed (the media out-point
	/// follows the new length). Undoable.
	///
	/// `length` is in rational seconds and is written as-is: there is no
	/// validation or clamping, so a negative or zero length produces an
	/// inverted or empty timeline range. Returns `false` when the project
	/// handle or the token is invalid.
	pub fn block_set_length(&self, block: NodeId, length: Rational) -> bool {
		unsafe { quercus_block_set_length(self.ptr, block.0, length) }
	}

	/// Sets the block's length, keeping the out point anchored in the
	/// media (left-edge resize). Undoable.
	pub fn block_set_length_and_media_in(&self, block: NodeId, length: Rational) -> bool {
		unsafe { quercus_block_set_length_and_media_in(self.ptr, block.0, length) }
	}

	/// Sets the block's media in-point, changing which part of the source is
	/// shown while keeping the timeline placement and length. Undoable.
	///
	/// `media_in` is in rational seconds on the source timeline. Returns
	/// `false` when the project handle or the token is invalid.
	pub fn block_set_media_in(&self, block: NodeId, media_in: Rational) -> bool {
		unsafe { quercus_block_set_media_in(self.ptr, block.0, media_in) }
	}

	/// Writes the static value of `id`'s input `input_id` as a single
	/// undoable parameter edit.
	///
	/// `element` is the array element index (`-1` = the scalar input).
	/// `value` is copied field-for-field and converted according to the
	/// input's declared type (see [`NodeValue`]).
	///
	/// # Returns
	///
	/// `false` when the node or named input does not exist, when the value
	/// type is incompatible with the input, or when the project handle is
	/// invalid; nothing is pushed onto the undo stack in those cases.
	pub fn set_standard_value(
		&self,
		id: NodeId,
		input_id: &str,
		element: i32,
		value: NodeValue,
	) -> bool {
		unsafe {
			quercus_node_set_standard_value(
				self.ptr,
				id.0,
				input_id.as_ptr(),
				input_id.len(),
				element,
				value,
			)
		}
	}

	/// Connects `from`'s output to `to`'s `input_id` at `element` (-1 =
	/// the scalar input). These edges are document structure (persisted
	/// in the project file).
	pub fn connect(&self, from: NodeId, to: NodeId, input_id: &str, element: i32) -> bool {
		unsafe {
			quercus_node_connect(self.ptr, from.0, to.0, input_id.as_ptr(), input_id.len(), element)
		}
	}

	/// Removes the connection feeding `to`'s input `input_id` at element
	/// index `element` (`-1` = the scalar input; `0..` selects an array
	/// element).
	///
	/// These edges are document structure (persisted in the project file);
	/// this is the counterpart of [`Project::connect`]. Returns `false` when
	/// the project handle, the token or the input id is invalid; an input
	/// with no incoming connection is a no-op that still returns `true`.
	pub fn disconnect(&self, to: NodeId, input_id: &str, element: i32) -> bool {
		unsafe { quercus_node_disconnect(self.ptr, to.0, input_id.as_ptr(), input_id.len(), element) }
	}

}

impl Default for Project {
	fn default() -> Self {
		Self::new()
	}
}

impl Clone for Project {
	fn clone(&self) -> Self {
		unsafe { quercus_project_add_ref(self.ptr) };
		Self { ptr: self.ptr }
	}
}

impl Drop for Project {
	fn drop(&mut self) {
		unsafe { quercus_project_release(self.ptr) };
	}
}

/// Sets (or with `None` clears) the process-wide active project — the
/// one plugins see through [`active_project`].
pub fn set_active_project(project: Option<&Project>) {
	unsafe {
		quercus_project_set_active(match project {
			Some(p) => p.ptr,
			None => std::ptr::null(),
		})
	};
}

/// The process-wide active project (`None` when the host has none open).
pub fn active_project() -> Option<Project> {
	let ptr = unsafe { quercus_project_active() };
	if ptr.is_null() {
		None
	} else {
		Some(Project { ptr })
	}
}


impl Project {
	/// Saves the project as XML to `path` (`None` = the project's own
	/// filename; save-as semantics: the path becomes the filename).
	/// Plugins are trusted in-process code — no path restrictions.
	pub fn save(&self, path: Option<&str>) -> bool {
		let (p, pl) = match path {
			Some(p) => (p.as_ptr(), p.len()),
			None => (std::ptr::null(), 0),
		};
		unsafe { quercus_project_save(self.ptr, p, pl) }
	}

	/// Loads a project from an XML file. Not made active automatically.
	pub fn load(path: &str) -> Option<Project> {
		let ptr = unsafe { quercus_project_load(path.as_ptr(), path.len()) };
		if ptr.is_null() {
			None
		} else {
			Some(Project { ptr })
		}
	}

	/// Serializes the project to its XML byte representation, the same
	/// document [`Project::save`] writes. Returns `None` on a serialization
	/// failure. The bytes are UTF-8 project XML with no trailing NUL and do
	/// not depend on the project's filename, so an unsaved project
	/// round-trips too.
	pub fn to_xml_bytes(&self) -> Option<Vec<u8>> {
		let n = unsafe { quercus_project_save_to_bytes(self.ptr, std::ptr::null_mut(), 0) };
		if n < 0 {
			return None;
		}
		let mut buf = vec![0u8; n as usize + 1];
		let written =
			unsafe { quercus_project_save_to_bytes(self.ptr, buf.as_mut_ptr(), buf.len()) };
		if written != n {
			// The project changed between the size query and the fetch.
			return None;
		}
		buf.truncate(n as usize);
		Some(buf)
	}

	/// Parses a project from its XML byte representation (as produced by
	/// [`Project::to_xml_bytes`] or [`Project::save`]).
	///
	/// On success returns `(project, version_info)`, where `version_info` is
	/// `0` (current), `1` (too old; upgraded on load), `2` (too new) or `3`
	/// (unknown version). For the latter two the project is `None` while
	/// `version_info` is still meaningful — the document was intact but this
	/// build cannot load it. A genuine failure is `Err(negative
	/// QUERCUSSTORAGE_E_* code)`.
	pub fn from_xml_bytes(bytes: &[u8]) -> Result<(Option<Project>, i32), i32> {
		let mut version_info = 0i32;
		let mut error = 0i32;
		let ptr = unsafe {
			quercus_project_load_from_bytes(
				bytes.as_ptr(),
				bytes.len(),
				&mut version_info,
				&mut error,
			)
		};
		if error != 0 {
			return Err(error);
		}
		let project = if ptr.is_null() {
			None
		} else {
			Some(Project { ptr })
		};
		Ok((project, version_info))
	}

	/// Sets the project file path (used by save when no path is given).
	pub fn set_filename(&self, path: &str) -> bool {
		unsafe { quercus_project_set_filename(self.ptr, path.as_ptr(), path.len()) }
	}

	/// Sets the working color space: persisted into the project settings
	/// AND applied to the process-wide color pipeline.
	pub fn set_working_color_space(&self, setting: &str) -> bool {
		unsafe { quercus_project_set_working_color_space(self.ptr, setting.as_ptr(), setting.len()) }
	}

	/// Sets the output gamut/transfer (persisted + applied to the
	/// pipeline).
	pub fn set_output_color_spec(&self, gamut: &str, transfer: &str) -> bool {
		unsafe {
			quercus_project_set_output_color_spec(
				self.ptr,
				gamut.as_ptr(),
				gamut.len(),
				transfer.as_ptr(),
				transfer.len(),
			)
		}
	}

	/// A deep copy of the project (background-render isolation).
	pub fn deep_copy(&self) -> Option<Project> {
		let ptr = unsafe { quercus_project_deep_copy(self.ptr) };
		if ptr.is_null() {
			None
		} else {
			Some(Project { ptr })
		}
	}
}

/// Opens a global undo group: every undoable edit until
/// [`undo_group_end`] (or [`undo_group_abort`]) is merged into one
/// history row named `name`.
///
/// `name` is the user-visible label for that row. Returns `false` when a
/// group is already open or `name` is invalid. The stack is
/// process-global, so all callers in the process share it.
pub fn undo_group_begin(name: &str) -> bool {
	unsafe { quercus_undo_group_begin(name.as_ptr(), name.len()) }
}

/// Closes the current undo group. False when none is open.
pub fn undo_group_end() -> bool {
	unsafe { quercus_undo_group_end() }
}

/// Aborts the current undo group, rolling it back. False when none is
/// open.
pub fn undo_group_abort() -> bool {
	unsafe { quercus_undo_group_abort() }
}

/// Undoes one step. False when the stack is empty.
pub fn undo() -> bool {
	unsafe { quercus_undo_undo() }
}

/// Redoes one step. False when nothing to redo.
pub fn redo() -> bool {
	unsafe { quercus_undo_redo() }
}

// ---------------------------------------------------------------------------
// Undo stack (read-only)
// ---------------------------------------------------------------------------

/// Whether the global undo stack can undo.
pub fn undo_can_undo() -> bool {
	unsafe { quercus_undo_can_undo() }
}

/// Whether the global undo stack can redo.
pub fn undo_can_redo() -> bool {
	unsafe { quercus_undo_can_redo() }
}

/// Number of commands on the undo stack.
pub fn undo_count() -> i64 {
	unsafe { quercus_undo_count() }
}

/// The current position on the undo stack.
pub fn undo_index() -> i64 {
	unsafe { quercus_undo_index() }
}

/// Jumps to history position `index` (undoing/redoing as needed).
pub fn undo_jump(index: i64) -> bool {
	unsafe { quercus_undo_jump(index) }
}

/// Clears the undo stack (leaving the fresh "new project" row).
pub fn undo_clear() -> bool {
	unsafe { quercus_undo_clear() }
}

/// The display name of history row `row`; None when out of range.
pub fn undo_command_name(row: i64) -> Option<String> {
	string_out(|buf, len| unsafe { quercus_undo_command_name(row, buf, len) })
}

/// Whether history row `row` is currently done; None when out of range.
pub fn undo_command_done(row: i64) -> Option<bool> {
	match unsafe { quercus_undo_command_done(row) } {
		0 => Some(false),
		1 => Some(true),
		_ => None,
	}
}

type UndoObserverFn = unsafe extern "C" fn(userdata: *mut c_void);

// Registered observers (double-boxed closures; the engine hands the
// pointer back as userdata). Registration-only, matching the engine.
static UNDO_OBSERVERS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// Registers a change observer: called after any undoable command lands
/// (push, undo, redo, jump, clear) — the "project modified" signal.
/// Registration-only (no removal).
pub fn add_undo_observer(f: impl Fn() + Send + 'static) {
	/// C callback that forwards one undo-observer notification to the boxed
	/// Rust closure registered by [`add_undo_observer`].
	///
	/// # Safety
	///
	/// `userdata` must be null or a pointer created by [`add_undo_observer`]
	/// (a `Box<Box<dyn Fn() + Send>>` kept alive in `UNDO_OBSERVERS`). The
	/// engine may call this from any thread after an undoable command lands,
	/// so the pointee must stay valid for the lifetime of the registration.
	unsafe extern "C" fn trampoline(userdata: *mut c_void) {
		if userdata.is_null() {
			return;
		}
		let f = unsafe { &**(userdata as *const Box<dyn Fn() + Send>) };
		f();
	}
	let boxed = Box::into_raw(Box::new(Box::new(f) as Box<dyn Fn() + Send>));
	UNDO_OBSERVERS
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.push(boxed as usize);
	unsafe { quercus_undo_add_observer(Some(trampoline), boxed as *mut c_void) };
}

/// Serializes every test that touches the process-global undo stack
/// (all test modules in this crate share it).
#[cfg(test)]
pub(crate) static UNDO_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
	use super::*;

	/// Layout assertions shared with `include/quercus_engine.h`.
	#[test]
	fn abi_mirror_layout_matches_c_header() {
		assert_eq!(std::mem::size_of::<NodeValue>(), 56);
		assert_eq!(
			std::mem::size_of::<crate::editor::StorageProjectHandle>(),
			32
		);
	}

	/// History navigation/inspection and the observer hook (the undo
	/// stack is process-global; serialize).

	#[test]
	fn undo_history_rows_and_observer() {
		let _guard = UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		static SEEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
		add_undo_observer(|| {
			SEEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
		});

		let project = Project::new();
		let base = undo_count();
		project.create_folder(project.root(), "Observed").expect("folder");
		assert_eq!(undo_count(), base + 1);
		assert!(SEEN.load(std::sync::atomic::Ordering::SeqCst) > 0);

		assert!(undo_command_name(base).is_some());
		assert_eq!(undo_command_done(base), Some(true));
		assert!(undo_jump(base));
		assert_eq!(undo_command_done(base), Some(false));
		assert!(undo_jump(base + 1));
		assert_eq!(undo_command_done(base), Some(true));

		assert!(undo_clear());
		assert_eq!(undo_count(), 1, "clear leaves the fresh empty row");
	}

	#[test]
	fn node_label_and_sequence_params() {
		let _guard = UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		let project = Project::new();
		let folder = project.create_folder(project.root(), "Labeled").expect("folder");
		assert_eq!(project.node_label(folder), "Labeled");
		assert!(project.node_set_label(folder, "Renamed"));
		assert_eq!(project.node_label(folder), "Renamed");
		assert!(!project.node_set_label(NodeId::INVALID, "x"));

		let seq = project.create_sequence("Seq").expect("sequence");
		// A fresh sequence carries the default 29.97 fps video params.
		assert_eq!(project.sequence_frame_rate(seq), Rational::new(30000, 1001));
		assert!(project.sequence_geometry(seq).is_some());
		assert!(project.sequence_set_video_params(seq, 1920, 1080, 24, 1));
		assert_eq!(project.sequence_frame_rate(seq), Rational::new(24, 1));
		assert_eq!(project.sequence_geometry(seq), Some((1920, 1080)));
		assert!(!project.sequence_set_video_params(NodeId::INVALID, 1, 1, 1, 1));
		assert!(undo_clear());
	}

	#[test]
	fn project_basics_and_node_ids() {
		let project = Project::new();
		assert!(!project.modified());
		assert!(project.filename().is_empty());

		let root = project.root();
		assert!(root.is_valid());
		assert!(!NodeId::INVALID.is_valid());
		assert_eq!(project.node_kind(root), Some(NodeKind::Folder));
		assert_eq!(project.node_kind(NodeId::INVALID), None);
		assert!(project
			.node_type_id(root)
			.is_some_and(|id| id.ends_with("folder")));
		assert_eq!(project.folder_child_count(root), 0);

		// Clone shares the project: same root.
		let clone = project.clone();
		assert_eq!(clone.root(), root);
	}

	#[test]
	fn active_project_slot() {
		set_active_project(None);
		assert!(active_project().is_none());

		let project = Project::new();
		set_active_project(Some(&project));
		let active = active_project().expect("active");
		assert_eq!(active.root(), project.root());

		set_active_project(None);
		assert!(active_project().is_none());
	}

	#[test]
	fn undo_read_only() {
		// Fresh process-wide stack state is whatever the suite left;
		// just verify the calls work.
		let _ = undo_can_undo();
		let _ = undo_can_redo();
		assert!(undo_count() >= 0);
	}

	#[test]
	fn unreadable_nodes_fail_cleanly() {
		let project = Project::new();
		let root = project.root();
		// The root folder has no "file_in" input; and no node reads work
		// on an invalid id.
		assert!(project.standard_value(root, "file_in", -1).is_none());
		assert!(project.standard_value(NodeId::INVALID, "file_in", -1).is_none());
		assert!(project
			.value_at_time(root, "file_in", -1, Rational::new(0, 1))
			.is_none());
		assert_eq!(project.footage_filename(root), None, "root is a folder");
		assert_eq!(project.track_block_count(root), 0);
		assert_eq!(project.sequence_track_list_at(root, 0), NodeId::INVALID);
	}
}

#[cfg(test)]
mod edit_tests {
	use super::*;
	use crate::codec::{Codec, EncoderSession, EncodingParams, Format};
	use crate::codec::Frame;

	// The undo stack is process-global; serialize the edit tests.

	/// A real 64x64 clip encoded through the SDK (the encode session
	/// doubles as test-media generation).
	fn make_test_clip(name: &str) -> String {
		let out = std::env::temp_dir().join(format!(
			"quercussdk_nodeedit_{name}_{}.mp4",
			std::process::id()
		));
		let mut params = EncodingParams::new();
		params.set_filename(out.to_str().unwrap()).unwrap();
		params.format = Format::MPEG4Video as i32;
		params.video_enabled = 1;
		params.video_codec = Codec::H264 as i32;
		params.video_width = 64;
		params.video_height = 64;
		params.video_time_base_num = 1;
		params.video_time_base_den = 10;
		params.video_pixel_format = crate::types::PixelFormat::F32 as i32;
		params.video_pixel_aspect_num = 1;
		params.video_pixel_aspect_den = 1;

		let session = EncoderSession::create(&params).expect("create encoder");
		session.open().expect("open");
		let mut vp = crate::types::VideoParams::new();
		vp.width = 64;
		vp.height = 64;
		vp.format = crate::types::PixelFormat::F32 as i32;
		vp.channel_count = 4;
		for i in 0..10 {
			let mut frame = Frame::with_params(&vp);
			frame.allocate().expect("allocate");
			frame.set_timestamp(Rational::new(i, 10));
			session.write_video(&frame).expect("write");
		}
		session.flush().expect("flush");
		session.close().expect("close");
		out.to_string_lossy().into_owned()
	}

	/// The full chain through the SDK: import → sequence → clip →
	/// place → trim → undo back → redo forward.
	#[test]
	fn sdk_edit_chain_and_undo() {
		let _guard = UNDO_SERIAL.lock().unwrap();
		let project = Project::new();

		// Import (encode a clip first: the SDK has no testmedia helper).
		let clip_path = make_test_clip("chain");
		let footage = project.import_footage(&clip_path).expect("import");
		assert_eq!(project.folder_child_count(project.root()), 1);
		assert!(project.footage_video_stream_count(footage) >= 1);
		let params = project.footage_video_params(footage, 0).expect("video params");
		assert_eq!((params.width, params.height), (64, 64));

		// Sequence + clip + placement.
		let seq = project.create_sequence("Seq 1").expect("sequence");
		assert_eq!(project.sequence_track_list_count(seq), 2);
		let tracklist = project.sequence_track_list_at(seq, 0);
		let track = project.tracklist_track_at(tracklist, 0);
		let clip = project.create_clip().expect("clip");
		assert_eq!(project.node_kind(clip), Some(NodeKind::Clip));
		assert!(project.track_append_block(track, clip));
		assert_eq!(project.track_block_count(track), 1);

		// Trim.
		assert!(project.block_set_length(clip, Rational::new(5, 1)));
		assert_eq!(project.block_length(clip), Rational::new(5, 1));

		// Undo: trim, placement, import (the sequence mount is creation,
		// not an edit).
		assert!(undo());
		assert_ne!(project.block_length(clip), Rational::new(5, 1));
		assert!(undo());
		assert_eq!(project.track_block_count(track), 0);
		assert!(undo());
		assert_eq!(project.folder_child_count(project.root()), 1, "only the sequence remains");
		assert_eq!(project.folder_child_at(project.root(), 0), seq);

		// Redo the whole chain.
		assert!(redo());
		assert!(redo());
		assert!(redo());
		assert_eq!(project.folder_child_count(project.root()), 2);
		assert_eq!(project.track_block_count(track), 1);
		assert_eq!(project.block_length(clip), Rational::new(5, 1));
	}

	#[test]
	fn sdk_undo_groups_batch_imports() {
		let _guard = UNDO_SERIAL.lock().unwrap();
		let project = Project::new();
		let clip_path = make_test_clip("group");
		let base = undo_count();

		assert!(undo_group_begin("import twice"));
		assert!(project.import_footage(&clip_path).is_some());
		assert!(undo_group_end());
		assert_eq!(undo_count(), base + 1, "the group is one undo step");
		assert!(undo());
		assert_eq!(project.folder_child_count(project.root()), 0);

		assert!(undo_group_begin("aborted"));
		assert!(project.import_footage(&clip_path).is_some());
		assert!(undo_group_abort());
		assert_eq!(project.folder_child_count(project.root()), 0);
		assert_eq!(undo_count(), base + 1, "abort records nothing");
	}
}

#[cfg(test)]
mod persist_tests {
	use super::*;


	#[test]
	fn save_load_roundtrip() {
		let _guard = UNDO_SERIAL.lock().unwrap();
		let project = Project::new();
		let folder = project
			.create_folder(project.root(), "Persisted")
			.expect("folder");

		let path = std::env::temp_dir()
			.join(format!("quercussdk_persist_{}.ove", std::process::id()))
			.to_string_lossy()
			.into_owned();
		assert!(project.save(Some(&path)));
		assert!(!project.modified());

		let loaded = Project::load(&path).expect("load");
		assert_eq!(loaded.filename(), path);
		assert_eq!(loaded.folder_child_count(loaded.root()), 1);
		assert_eq!(loaded.folder_child_at(loaded.root(), 0), folder);

		assert!(Project::load("/nonexistent/quercus-sdk.ove").is_none());
		let _ = std::fs::remove_file(&path);
	}

	#[test]
	fn xml_bytes_roundtrip_and_version_info() {
		let _guard = UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		let project = Project::new();
		project
			.create_folder(project.root(), "Bytes")
			.expect("folder");
		project.set_setting("byte-test", "1");

		let bytes = project.to_xml_bytes().expect("serialize");
		assert!(bytes.starts_with(b"<project"), "the bytes are project XML");

		let (loaded, version_info) = Project::from_xml_bytes(&bytes).expect("parse");
		let loaded = loaded.expect("loadable document");
		assert_eq!(version_info, 0);
		assert_eq!(loaded.uuid(), project.uuid());
		assert_eq!(loaded.folder_child_count(loaded.root()), 1);
		assert_eq!(loaded.setting("byte-test").as_deref(), Some("1"));
		assert!(loaded.filename().is_empty(), "bytes carry no filename");

		// Newer/unknown documents parse but report a version code without
		// an error, and yield no project.
		let (too_new, version_info) =
			Project::from_xml_bytes(b"<olive version=\"999999\"></olive>").expect("probe");
		assert!(too_new.is_none());
		assert_eq!(version_info, 2);
		let (unknown, version_info) =
			Project::from_xml_bytes(b"<thirdparty></thirdparty>").expect("probe");
		assert!(unknown.is_none());
		assert_eq!(version_info, 3);

		// Genuine failures surface the negative engine error code.
		assert!(Project::from_xml_bytes(b"not xml").is_err());
	}

	#[test]
	fn settings_sync_pipeline() {
		let project = Project::new();
		let saved = crate::state::color_working_space();
		assert!(project.set_working_color_space("srgb_legacy"));
		assert_eq!(crate::state::color_working_space(), "srgb_legacy");
		assert!(project.set_working_color_space(&saved));

		assert!(project.set_output_color_spec("displayp3", "srgb"));
		assert_eq!(crate::state::color_output_gamut(), "displayp3");
		project.set_output_color_spec("srgb", "srgb");
	}

	#[test]
	fn deep_copy_is_independent() {
		let _guard = UNDO_SERIAL.lock().unwrap();
		let project = Project::new();
		let folder = project.create_folder(project.root(), "CopyTest").expect("folder");

		let copy = project.deep_copy().expect("deep copy");
		assert_eq!(copy.folder_child_count(copy.root()), 1);

		assert!(copy.remove_node(folder));
		assert_eq!(copy.folder_child_count(copy.root()), 0);
		assert_eq!(project.folder_child_count(project.root()), 1);
	}
}
