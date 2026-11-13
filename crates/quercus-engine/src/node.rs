//! C ABI for quercus-node's document model: the active project, node
//! identity, and read-only structure traversal.
//!
//! Nodes are addressed by `NodeId::identity()` tokens (u64);
//! `QUERCUS_NODE_ID_INVALID` (u32::MAX) is the null token. Everything takes
//! the project handle explicitly — plugins read the active project via
//! [`quercus_project_active`]. All entry points are null-safe and never
//! panic across the boundary.

use std::sync::{Arc, Mutex, RwLock};

use quercus_core::Rational;
use quercus_node::block::{AdjustmentBlockBehavior, ClipBlockBehavior, GapBlockBehavior, TransitionBlockBehavior};
use quercus_node::footage::FootageBehavior;
use quercus_node::folder::FolderBehavior;
use quercus_node::id::NodeId;
use quercus_node::project::Project;
use quercus_node::sequence::SequenceBehavior;
use quercus_node::track::{TrackBehavior, TrackListBehavior};
use quercus_storage::error::{
	QUERCUSSTORAGE_E_FORMAT, QUERCUSSTORAGE_E_INVALID, QUERCUSSTORAGE_OK,
	QUERCUSSTORAGE_TOO_NEW, QUERCUSSTORAGE_TOO_OLD, QUERCUSSTORAGE_UNKNOWN_VERSION,
};
use quercus_timeline::util::GraphBlockRange;

use crate::codec::{copy_str_out, str_arg};
use crate::coretypes::QuercusRational;
use crate::handle::NativeMirror;

/// The null node token (`NodeId::INVALID`'s identity).
pub const QUERCUS_NODE_ID_INVALID: u64 = u32::MAX as u64;

/// The null node token.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_node_id_invalid() -> u64 {
	QUERCUS_NODE_ID_INVALID
}

/// Whether `id` is a well-formed node token.
///
/// Tokens are `NodeId` identities: the low 32 bits are the arena slot and the upper
/// 32 bits a generation counter. This checks only the encoding — every token whose
/// slot index is not `u32::MAX` passes, including a stale token whose node no
/// longer exists (`QUERCUS_NODE_ID_INVALID` itself is slot `u32::MAX`, generation
/// 0). Use `quercus_node_kind` or the traversal getters to test liveness.
///
/// # Returns
///
/// `true` when the token decodes, `false` for the invalid token.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_node_id_is_valid(id: u64) -> bool {
	NodeId::from_identity(id).is_some()
}

// ---------------------------------------------------------------------------
// Project handle + the active-project slot
// ---------------------------------------------------------------------------

/// An open project (refcounted handle).
pub struct ProjectHandle {
	inner: Arc<Mutex<Project>>,
}

/// Converts an owned engine project handle into the storage migration handle used
/// by legacy hosts.
///
/// The handle's shared `Arc<Mutex<Project>>` is moved into the returned `CHandle`
/// (the same project, not a reloaded copy), and the input reference is released —
/// this function consumes exactly one reference, so the caller must not use
/// `project` or release it again afterwards. A null input returns a null `CHandle`
/// and releases nothing.
///
/// # Safety
///
/// `project` must be null or a valid `ProjectHandle*` that the caller owns and is
/// transferring; after the call the pointer is dangling.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_into_storage_handle(
	project: *mut ProjectHandle,
) -> quercus_core::handle::CHandle {
	let Some(project) = (unsafe { project.as_ref() }) else {
		return quercus_core::handle::CHandle::null();
	};
	let inner = project.project().clone();
	unsafe { quercus_project_release(project) };
	quercus_storage::nodeutil::make_project_owned(inner)
}

impl ProjectHandle {
	/// Wraps an existing shared project as a handle with ref count 1
	/// (the storage backend boundary hands projects over as
	/// `Arc<Mutex<Project>>`).
	pub(crate) fn from_arc(inner: Arc<Mutex<Project>>) -> *mut ProjectHandle {
		crate::handle::into_ffi(ProjectHandle { inner }) as *mut ProjectHandle
	}

	/// The shared project (crate-internal consumers: storage save,
	/// frame rendering).
	pub(crate) fn project(&self) -> &Arc<Mutex<Project>> {
		&self.inner
	}
}

crate::export_handle!(
	ProjectHandle,
	quercus_project_add_ref,
	quercus_project_release
);

/// The process-wide active project slot: the host sets it when a project
/// opens, plugins read it via [`quercus_project_active`].
static ACTIVE_PROJECT: RwLock<Option<Arc<Mutex<Project>>>> = RwLock::new(None);

/// A new, initialized project (empty graph with a root folder), ref
/// count 1.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_project_new() -> *mut ProjectHandle {
	let project = Project::new();
	project
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.initialize()
		.ok();
	crate::handle::into_ffi(ProjectHandle { inner: project }) as *mut ProjectHandle
}

/// Sets (or with null clears) the process-wide active project slot that
/// `quercus_project_active` reads.
///
/// The slot stores a clone of the handle's shared `Arc`, so the caller keeps its
/// own reference and must release it normally; replacing or clearing the slot drops
/// the previously stored reference. The slot is protected by a process-wide
/// `RwLock` and poisoned locks are recovered, so the call cannot panic.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration of
/// the call; the handle's `Arc` is cloned into the slot, not the raw pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_set_active(project: *const ProjectHandle) {
	let slot = if project.is_null() {
		None
	} else {
		Some(unsafe { &*project }.inner.clone())
	};
	*ACTIVE_PROJECT.write().unwrap_or_else(|e| e.into_inner()) = slot;
}

/// A new reference on the active project (null when none is set).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_project_active() -> *mut ProjectHandle {
	let slot = ACTIVE_PROJECT.read().unwrap_or_else(|e| e.into_inner()).clone();
	match slot {
		Some(inner) => crate::handle::into_ffi(ProjectHandle { inner }) as *mut ProjectHandle,
		None => std::ptr::null_mut(),
	}
}

/// Runs `f` with shared access to the project behind `project`, holding the project
/// mutex for the whole call.
///
/// Returns `None` without calling `f` when `project` is null; otherwise returns
/// `f`'s result. Poisoned locks are recovered with `into_inner`, so this cannot
/// panic on a poisoned lock. Read-only getters in this module are built on it, and
/// `f` must not re-enter another entry point that locks the same project (that
/// would deadlock).
pub(crate) fn with_project<R>(
	project: *const ProjectHandle,
	f: impl FnOnce(&Project) -> R,
) -> Option<R> {
	if project.is_null() {
		return None;
	}
	let guard = unsafe { &*project }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	Some(f(&guard))
}

fn node_id(token: u64) -> Option<NodeId> {
	NodeId::from_identity(token)
}

/// Clones the project's shared `Arc` out of its handle (for manager APIs
/// that take the same `Arc<Mutex<Project>>` the app and workers share).
pub(crate) fn project_arc(project: *const ProjectHandle) -> Option<Arc<Mutex<Project>>> {
	if project.is_null() {
		return None;
	}
	Some(unsafe { &*project }.inner.clone())
}

/// Borrowed downcast helper: the behaviour of the node at `id` as `&T`.
///
/// Returns `None` when `id` names no live node or when the node's behaviour is a
/// different concrete type (for example a footage node queried as a
/// `TrackBehavior`). The typed getters in this module use it so a wrong node kind
/// reports the documented fallback instead of a fabricated value.
fn behavior_as<T: 'static>(project: &Project, id: NodeId) -> Option<&T> {
	project
		.graph
		.get(id)?
		.behavior
		.as_any()?
		.downcast_ref::<T>()
}

/// Shared tail of the snprintf-style string getters: renders `Some(value)` through
/// `f` into `buf`, and returns `-1` for `None`.
///
/// `f` receives `&value` plus the unchanged `(buf, buf_len)` pair and must follow
/// the `copy_str_out` convention (NUL-terminate, return the full byte length, and
/// treat a null buffer as a size query). The getters in this module use it to turn
/// an unavailable value (null handle, unknown/stale id, wrong node kind) into the
/// conventional `-1`.
fn write_str<R: Default>(value: Option<R>, f: impl FnOnce(&R, *mut u8, usize) -> i32, buf: *mut u8, buf_len: usize) -> i32 {
	match value {
		Some(v) => f(&v, buf, buf_len),
		None => -1,
	}
}

macro_rules! project_string_getter {
	($( $name:ident => |$p:ident| $body:expr ),* $(,)?) => {
		$(
			#[doc = "Snprintf-style project string getter; -1 on null/missing."]
			#[unsafe(no_mangle)]
			pub unsafe extern "C" fn $name(
				project: *const ProjectHandle,
				buf: *mut u8,
				buf_len: usize,
			) -> i32 {
				write_str(
					with_project(project, |$p| $body).flatten(),
					|s, buf, len| copy_str_out(s, buf, len),
					buf,
					buf_len,
				)
			}
		)*
	};
}

project_string_getter! {
	quercus_project_name => |p| Some(p.name()),
	quercus_project_filename => |p| Some(p.filename.clone()),
	quercus_project_pretty_filename => |p| Some(p.pretty_filename().to_string()),
}

/// Whether the project has unsaved changes (its dirty flag).
///
/// The flag is set by the writing helpers that call `Project::set_modified` (for
/// example `quercus_project_set_setting`, `quercus_project_set_cache_location` and
/// `quercus_project_unset_setting`) and cleared by a successful
/// `quercus_project_save`.
///
/// # Returns
///
/// `false` for a null handle as well as for a clean project; the two cases are not
/// distinguished.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_modified(project: *const ProjectHandle) -> bool {
	with_project(project, |p| p.modified).unwrap_or(false)
}

/// The root folder node token of the project's bin tree.
///
/// This is the entry point for the folder traversal getters and a valid `parent`
/// argument for `quercus_folder_create`.
///
/// # Returns
///
/// `QUERCUS_NODE_ID_INVALID` for a null handle or an uninitialized project
/// (`Project::root` is the invalid id until initialization).
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_root(project: *const ProjectHandle) -> u64 {
	with_project(project, |p| p.root.identity()).unwrap_or(QUERCUS_NODE_ID_INVALID)
}

// ---------------------------------------------------------------------------
// Generic node queries
// ---------------------------------------------------------------------------

/// The node's type id string (for example
/// `"org.olivevideoeditor.Olive.clipblock"`), snprintf-style.
///
/// This is the factory type id, the same string `quercus_generator_create` accepts
/// for generators. Follows the crate's buffer convention: the full byte length
/// excluding the NUL is returned, a null `buf` (or `buf_len == 0`) is a size query,
/// and `ret >= buf_len` means truncation.
///
/// # Returns
///
/// `-1` for a null handle or a malformed/stale id.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `buf` must be null
/// or writable for `buf_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_node_type_id(
	project: *const ProjectHandle,
	id: u64,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let value = node_id(id).and_then(|id| {
		with_project(project, |p| {
			p.graph.get(id).map(|e| e.behavior.type_id().to_string())
		})
		.flatten()
	});
	write_str(value, |s, b, l| copy_str_out(s, b, l), buf, buf_len)
}

/// The node's coarse kind code: 1 folder, 2 footage, 3 sequence, 4 track, 5 clip,
/// 6 gap, 7 transition, 8 adjustment, 9 track list; 0 unknown.
///
/// The code is derived from the node's type id string, so it is the cheap kind
/// test to use before calling the typed getters. Generator and effect nodes are not
/// in the list and report 0.
///
/// # Returns
///
/// The code, or `0` for a null handle, a malformed/stale id, or an unknown type.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_node_kind(project: *const ProjectHandle, id: u64) -> i32 {
	let Some(id) = node_id(id) else { return 0 };
	with_project(project, |p| {
		p.graph.get(id).map(|e| {
			match e.behavior.type_id() {
				"org.olivevideoeditor.Olive.folder" => 1,
				"org.olivevideoeditor.Olive.footage" => 2,
				"org.olivevideoeditor.Olive.sequence" => 3,
				"org.olivevideoeditor.Olive.track" => 4,
				"org.olivevideoeditor.Olive.clipblock" => 5,
				"org.olivevideoeditor.Olive.gapblock" => 6,
				"org.olivevideoeditor.Olive.transitionblock" => 7,
				"org.olivevideoeditor.Olive.adjustment" => 8,
				"org.olivevideoeditor.Olive.tracklist" => 9,
				_ => 0,
			}
		})
	})
	.flatten()
	.unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Folder
// ---------------------------------------------------------------------------

/// Number of children in a folder node (the bin tree).
///
/// # Returns
///
/// The count, or `0` for a null handle, a malformed/stale id, or a non-folder node.
/// Pair with `quercus_folder_child_at`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_folder_child_count(project: *const ProjectHandle, id: u64) -> i32 {
	let Some(id) = node_id(id) else { return 0 };
	with_project(project, |p| {
		behavior_as::<FolderBehavior>(p, id).map(|f| f.children.len() as i32)
	})
	.flatten()
	.unwrap_or(0)
}

/// The `index`-th child of a folder node, in bin-tree order.
///
/// `index` is a 0-based position; pair with `quercus_folder_child_count` to
/// iterate the children.
///
/// # Returns
///
/// The child token, or `QUERCUS_NODE_ID_INVALID` for a negative/out-of-range
/// `index`, a null handle, a malformed id, or a non-folder node.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_folder_child_at(
	project: *const ProjectHandle,
	id: u64,
	index: i32,
) -> u64 {
	let Some(id) = node_id(id) else { return QUERCUS_NODE_ID_INVALID };
	if index < 0 {
		return QUERCUS_NODE_ID_INVALID;
	}
	with_project(project, |p| {
		behavior_as::<FolderBehavior>(p, id)
			.and_then(|f| f.children.get(index as usize).copied())
			.map(|c| c.identity())
	})
	.flatten()
	.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

// ---------------------------------------------------------------------------
// Footage
// ---------------------------------------------------------------------------

/// A video stream's parameters as carried by a footage node.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusStreamVideoParams {
	pub width: i32,
	pub height: i32,
	pub frame_rate: QuercusRational,
	pub pixel_format: i32,
	pub channels: i32,
	pub interlaced: i32,
}

impl NativeMirror for QuercusStreamVideoParams {
	type Native = quercus_node::value::VideoParams;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			width: native.width,
			height: native.height,
			frame_rate: QuercusRational::from_native(&native.frame_rate),
			pixel_format: native.pixel_format,
			channels: native.channels,
			interlaced: i32::from(native.interlaced),
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			width: self.width,
			height: self.height,
			frame_rate: self.frame_rate.to_native(),
			pixel_format: self.pixel_format,
			channels: self.channels,
			interlaced: self.interlaced != 0,
		}
	}
}

/// An audio stream's parameters as carried by a footage node.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusStreamAudioParams {
	pub sample_rate: i32,
	pub channel_layout: u64,
	pub format: i32,
}

impl NativeMirror for QuercusStreamAudioParams {
	type Native = quercus_node::value::AudioParams;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			sample_rate: native.sample_rate,
			channel_layout: native.channel_layout,
			format: native.format,
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			sample_rate: self.sample_rate,
			channel_layout: self.channel_layout,
			format: self.format,
		}
	}
}

/// The footage's file path as stored on the node, snprintf-style.
///
/// The path is what the import stored (absolute when the caller passed an absolute
/// path, otherwise the relative path as given). Follows the crate's buffer
/// convention: the full byte length excluding the NUL is returned, a null `buf` (or
/// `buf_len == 0`) is a size query, and `ret >= buf_len` means truncation.
///
/// # Returns
///
/// `-1` for a null handle, a malformed/stale id, or a node that is not footage.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `buf` must be null
/// or writable for `buf_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_footage_filename(
	project: *const ProjectHandle,
	id: u64,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let value = node_id(id).and_then(|id| {
		with_project(project, |p| {
			behavior_as::<FootageBehavior>(p, id).map(|f| f.filename.clone())
		})
		.flatten()
	});
	write_str(value, |s, b, l| copy_str_out(s, b, l), buf, buf_len)
}

macro_rules! footage_count {
	($( $name:ident => $native:ident ),* $(,)?) => {
		$(
			#[doc = concat!("`FootageBehavior::", stringify!($native), "` (0 on null/wrong kind).")]
			#[unsafe(no_mangle)]
			pub unsafe extern "C" fn $name(project: *const ProjectHandle, id: u64) -> i32 {
				let Some(id) = node_id(id) else { return 0 };
				with_project(project, |p| {
					behavior_as::<FootageBehavior>(p, id).map(|f| f.$native() as i32)
				})
				.flatten()
				.unwrap_or(0)
			}
		)*
	};
}

footage_count! {
	quercus_footage_total_stream_count => total_stream_count,
	quercus_footage_video_stream_count => video_stream_count,
	quercus_footage_audio_stream_count => audio_stream_count,
	quercus_footage_subtitle_stream_count => subtitle_stream_count,
}

macro_rules! footage_rational {
	($( $name:ident => $native:ident ),* $(,)?) => {
		$(
			#[doc = concat!("`FootageBehavior::", stringify!($native), "` (null rational on null/wrong kind).")]
			#[unsafe(no_mangle)]
			pub unsafe extern "C" fn $name(project: *const ProjectHandle, id: u64) -> QuercusRational {
				let null = QuercusRational { num: 0, den: 0 };
				let Some(id) = node_id(id) else { return null };
				with_project(project, |p| {
					behavior_as::<FootageBehavior>(p, id)
						.map(|f| QuercusRational::from_native(&f.$native()))
				})
				.flatten()
				.unwrap_or(null)
			}
		)*
	};
}

footage_rational! {
	quercus_footage_duration => duration,
	quercus_footage_video_length => video_length,
}

/// Reads the footage's optional source-start timecode into `*out`.
///
/// The value exists only when the footage was probed with a known source-start
/// timecode (`has_source_start_time`), which is why this returns a flag instead of
/// writing a null rational. The written value is the `Rational` timecode embedded
/// in the media.
///
/// # Returns
///
/// `true` with `*out` written when a source start exists; `false` (leaving `*out`
/// untouched) for a null `out` or handle, a malformed/stale id, a node that is not
/// footage, or footage without a source-start timecode.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `out` must be null
/// or writable for one `QuercusRational`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_footage_source_start_time(
	project: *const ProjectHandle,
	id: u64,
	out: *mut QuercusRational,
) -> bool {
	let (Some(id), Some(out)) = (node_id(id), unsafe { out.as_mut() }) else {
		return false;
	};
	let value = with_project(project, |project| {
		behavior_as::<FootageBehavior>(project, id).and_then(|footage| {
			footage
				.has_source_start_time
				.then_some(QuercusRational::from_native(&footage.source_start_time))
		})
	})
	.flatten();
	let Some(value) = value else {
		return false;
	};
	*out = value;
	true
}

/// Copies the `ordinal`-th video stream's parameters into `*out`.
///
/// `ordinal` is a 0-based index into the footage's video stream list (probe
/// order), not a global stream index. The written struct holds width, height,
/// frame rate, pixel format, channel count and the interlaced flag.
///
/// # Returns
///
/// `true` with `*out` written on success; `false` for a negative `ordinal`, a null
/// `out` or handle, a malformed/stale id, a node that is not footage, or an
/// `ordinal` outside the video stream list.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `out` must be null
/// or writable for one `QuercusStreamVideoParams`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_footage_video_params(
	project: *const ProjectHandle,
	id: u64,
	ordinal: i32,
	out: *mut QuercusStreamVideoParams,
) -> bool {
	if out.is_null() || ordinal < 0 {
		return false;
	}
	let Some(id) = node_id(id) else { return false };
	let params = with_project(project, |p| {
		behavior_as::<FootageBehavior>(p, id)
			.and_then(|f| f.video_params(ordinal as usize))
			.map(|v| QuercusStreamVideoParams::from_native(&v))
	})
	.flatten();
	match params {
		Some(v) => {
			unsafe { *out = v };
			true
		}
		None => false,
	}
}

/// Copies the `ordinal`-th audio stream's parameters into `*out`.
///
/// `ordinal` is a 0-based index into the footage's audio stream list (probe
/// order), not a global stream index. The written struct holds the sample rate,
/// channel layout and sample format.
///
/// # Returns
///
/// `true` with `*out` written on success; `false` for a negative `ordinal`, a null
/// `out` or handle, a malformed/stale id, a node that is not footage, or an
/// `ordinal` outside the audio stream list.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `out` must be null
/// or writable for one `QuercusStreamAudioParams`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_footage_audio_params(
	project: *const ProjectHandle,
	id: u64,
	ordinal: i32,
	out: *mut QuercusStreamAudioParams,
) -> bool {
	if out.is_null() || ordinal < 0 {
		return false;
	}
	let Some(id) = node_id(id) else { return false };
	let params = with_project(project, |p| {
		behavior_as::<FootageBehavior>(p, id)
			.and_then(|f| f.audio_params(ordinal as usize))
			.map(|a| QuercusStreamAudioParams::from_native(&a))
	})
	.flatten();
	match params {
		Some(v) => {
			unsafe { *out = v };
			true
		}
		None => false,
	}
}

/// Copies the footage's custom proxy parameters into `*out`.
///
/// Only the per-footage override is returned, not the global defaults, so a
/// `false` return means "no custom override exists". Pair with
/// `quercus_footage_effective_proxy_params` to read the value actually used for
/// rendering.
///
/// # Returns
///
/// `true` with `*out` written when an override exists; `false` (leaving `*out`
/// untouched) for a null `out`, a null handle, a malformed/stale id, a node that
/// is not footage, or footage without a custom override.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `out` must be null
/// or writable for one `ProxyParams`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_footage_custom_proxy_params(
	project: *const ProjectHandle,
	id: u64,
	out: *mut quercus_codec::proxymanager::ProxyParams,
) -> bool {
	if out.is_null() {
		return false;
	}
	let Some(id) = node_id(id) else { return false };
	with_project(project, |p| {
		behavior_as::<FootageBehavior>(p, id).and_then(|f| f.custom_proxy_params.clone())
	})
	.flatten()
	.map(|params| {
		unsafe { *out = params };
		true
	})
	.unwrap_or(false)
}

/// Copies the footage's effective proxy parameters into `*out`.
///
/// The effective values are the footage's custom override when one is set, and the
/// process-wide default proxy settings otherwise — exactly what a proxy render of
/// this footage would use. `*out` is a `ProxyParams` struct.
///
/// # Returns
///
/// `true` with `*out` written on success; `false` (leaving `*out` untouched) for a
/// null `out`, a null handle, a malformed/stale id, or a node that is not footage.
/// Pair with `quercus_footage_custom_proxy_params` to distinguish an override from
/// the defaults.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `out` must be null
/// or writable for one `ProxyParams`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_footage_effective_proxy_params(
	project: *const ProjectHandle,
	id: u64,
	out: *mut quercus_codec::proxymanager::ProxyParams,
) -> bool {
	if out.is_null() {
		return false;
	}
	let Some(id) = node_id(id) else { return false };
	with_project(project, |p| {
		behavior_as::<FootageBehavior>(p, id).map(|f| f.effective_proxy_params())
	})
	.flatten()
	.map(|params| {
		unsafe { *out = params };
		true
	})
	.unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Sequence / track list / track
// ---------------------------------------------------------------------------

/// Number of track lists in a sequence (video, audio and subtitle lists).
///
/// # Returns
///
/// The count, or `0` for a null handle, a malformed/stale id, or a non-sequence
/// node. Pair with `quercus_sequence_track_list_at`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_sequence_track_list_count(
	project: *const ProjectHandle,
	id: u64,
) -> i32 {
	let Some(id) = node_id(id) else { return 0 };
	with_project(project, |p| {
		behavior_as::<SequenceBehavior>(p, id).map(|s| s.track_lists.len() as i32)
	})
	.flatten()
	.unwrap_or(0)
}

/// The `index`-th track list of a sequence.
///
/// Lists are indexed in the order the sequence stores them; the default creation
/// layout appends video lists first, then audio lists. Pair with
/// `quercus_sequence_track_list_count`.
///
/// # Returns
///
/// The track-list token, or `QUERCUS_NODE_ID_INVALID` for a negative/out-of-range
/// `index`, a null handle, a malformed id, or a non-sequence node.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_sequence_track_list_at(
	project: *const ProjectHandle,
	id: u64,
	index: i32,
) -> u64 {
	let Some(id) = node_id(id) else { return QUERCUS_NODE_ID_INVALID };
	if index < 0 {
		return QUERCUS_NODE_ID_INVALID;
	}
	with_project(project, |p| {
		behavior_as::<SequenceBehavior>(p, id)
			.and_then(|s| s.track_lists.get(index as usize).copied())
			.map(|t| t.identity())
	})
	.flatten()
	.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// The sequence's playhead position as a rational time in seconds.
///
/// # Returns
///
/// The playhead, or the null rational `0/0` for a null handle, a malformed/stale
/// id, or a non-sequence node. Pair with `quercus_sequence_length` and
/// `quercus_sequence_frame_rate`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_sequence_playhead(
	project: *const ProjectHandle,
	id: u64,
) -> QuercusRational {
	let null = QuercusRational { num: 0, den: 0 };
	let Some(id) = node_id(id) else { return null };
	with_project(project, |p| {
		behavior_as::<SequenceBehavior>(p, id)
			.map(|s| QuercusRational::from_native(&s.playhead))
	})
	.flatten()
	.unwrap_or(null)
}

/// The track list's media type as a `TrackType` code: `0` video, `1` audio, `2`
/// subtitle.
///
/// # Returns
///
/// The code, or `-1` for a null handle, a malformed/stale id, or a non-track-list
/// node. Pair with `quercus_tracklist_track_count`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_tracklist_kind(project: *const ProjectHandle, id: u64) -> i32 {
	let Some(id) = node_id(id) else { return -1 };
	with_project(project, |p| {
		behavior_as::<TrackListBehavior>(p, id).map(|t| t.kind.to_c())
	})
	.flatten()
	.unwrap_or(-1)
}

/// Number of tracks in a track list.
///
/// # Returns
///
/// The count, or `0` for a null handle, a malformed/stale id, or a non-track-list
/// node. Pair with `quercus_tracklist_track_at`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_tracklist_track_count(
	project: *const ProjectHandle,
	id: u64,
) -> i32 {
	let Some(id) = node_id(id) else { return 0 };
	with_project(project, |p| {
		behavior_as::<TrackListBehavior>(p, id).map(|t| t.tracks.len() as i32)
	})
	.flatten()
	.unwrap_or(0)
}

/// The `index`-th track of a track list, in stack order.
///
/// `index` is a 0-based position in the list's track stack; pair with
/// `quercus_tracklist_track_count`.
///
/// # Returns
///
/// The track token, or `QUERCUS_NODE_ID_INVALID` for a negative/out-of-range
/// `index`, a null handle, a malformed id, or a non-track-list node.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_tracklist_track_at(
	project: *const ProjectHandle,
	id: u64,
	index: i32,
) -> u64 {
	let Some(id) = node_id(id) else { return QUERCUS_NODE_ID_INVALID };
	if index < 0 {
		return QUERCUS_NODE_ID_INVALID;
	}
	with_project(project, |p| {
		behavior_as::<TrackListBehavior>(p, id)
			.and_then(|t| t.tracks.get(index as usize).copied())
			.map(|t| t.identity())
	})
	.flatten()
	.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// The track's media type as a `TrackType` code: `0` video, `1` audio, `2`
/// subtitle.
///
/// # Returns
///
/// The code, or `-1` for a null handle, a malformed/stale id, or a non-track node.
/// The code is the same one `quercus_track_add` accepts.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_kind(project: *const ProjectHandle, id: u64) -> i32 {
	let Some(id) = node_id(id) else { return -1 };
	with_project(project, |p| {
		behavior_as::<TrackBehavior>(p, id).map(|t| t.kind.to_c())
	})
	.flatten()
	.unwrap_or(-1)
}

/// Number of blocks on a track.
///
/// Blocks are kept in timeline order; pair with `quercus_track_block_at` to walk
/// them.
///
/// # Returns
///
/// The block count, or `0` for a null handle, a malformed/stale id, or a non-track
/// node.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_block_count(project: *const ProjectHandle, id: u64) -> i32 {
	let Some(id) = node_id(id) else { return 0 };
	with_project(project, |p| {
		behavior_as::<TrackBehavior>(p, id).map(|t| t.blocks.len() as i32)
	})
	.flatten()
	.unwrap_or(0)
}

/// The `index`-th block on a track, in timeline order.
///
/// `index` is a 0-based position in the track's ordered block list; pair with
/// `quercus_track_block_count` to iterate.
///
/// # Returns
///
/// The block token, or `QUERCUS_NODE_ID_INVALID` for a negative/out-of-range
/// `index`, a null handle, a malformed id, or a non-track node.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_block_at(
	project: *const ProjectHandle,
	id: u64,
	index: i32,
) -> u64 {
	let Some(id) = node_id(id) else { return QUERCUS_NODE_ID_INVALID };
	if index < 0 {
		return QUERCUS_NODE_ID_INVALID;
	}
	with_project(project, |p| {
		behavior_as::<TrackBehavior>(p, id)
			.and_then(|t| t.block_at(index as usize))
			.map(|b| b.identity())
	})
	.flatten()
	.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// The block on this track that strictly contains `time`.
///
/// "Strictly" is exclusive on both ends: a block matches only when
/// `block.in < time < block.out`, so neither a block's exact start instant nor its
/// exact end instant selects it, and abutting blocks leave their shared boundary
/// unmatched. `time` is a rational time in seconds.
///
/// # Returns
///
/// The containing block token, or `QUERCUS_NODE_ID_INVALID` when no block matches
/// or the handle/id is unavailable.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_block_containing_time(
	project: *const ProjectHandle,
	id: u64,
	time: QuercusRational,
) -> u64 {
	let Some(id) = node_id(id) else { return QUERCUS_NODE_ID_INVALID };
	with_project(project, |p| {
		behavior_as::<TrackBehavior>(p, id).and_then(|t| {
			t.block_containing_time(time.to_native(), &GraphBlockRange { graph: &p.graph })
				.map(|b| b.identity())
		})
	})
	.flatten()
	.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// The track's total length: the end of its last block, in rational seconds.
///
/// Unlike most rational getters in this file, the unavailable value here is `0/1`
/// (a plain zero), not the `0/0` null rational: a null handle, a malformed/stale
/// id, a non-track node and an empty track all return `0/1`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_length(project: *const ProjectHandle, id: u64) -> QuercusRational {
	let null = QuercusRational { num: 0, den: 1 };
	let Some(id) = node_id(id) else { return null };
	with_project(project, |p| {
		behavior_as::<TrackBehavior>(p, id).map(|t| {
			QuercusRational::from_native(&t.length(&GraphBlockRange { graph: &p.graph }))
		})
	})
	.flatten()
	.unwrap_or(null)
}

// ---------------------------------------------------------------------------
// Block (clip/gap/transition/adjustment — anything with a BlockCore)
// ---------------------------------------------------------------------------

/// Borrowed access to a node's `BlockCore` when the node is a block.
///
/// Tries the clip, gap, transition and adjustment behaviours in turn and returns
/// the first match, so this is the common accessor behind the `quercus_block_*`
/// getters and `quercus_block_enabled`.
///
/// # Returns
///
/// `Some(&BlockCore)` for a live block node, `None` for a missing node or any other
/// node type.
fn block_core<'a>(
	project: &'a Project,
	id: NodeId,
) -> Option<&'a quercus_node::block::BlockCore> {
	let any = project.graph.get(id)?.behavior.as_any()?;
	[
		any.downcast_ref::<ClipBlockBehavior>().map(|b| &b.core),
		any.downcast_ref::<GapBlockBehavior>().map(|b| &b.core),
		any.downcast_ref::<TransitionBlockBehavior>().map(|b| &b.core),
		any.downcast_ref::<AdjustmentBlockBehavior>().map(|b| &b.core),
	]
	.into_iter()
	.flatten()
	.next()
}

macro_rules! block_rational {
	($( $name:ident => |$c:ident| $body:expr ),* $(,)?) => {
		$(
			#[doc = "BlockCore read (null rational on null/non-block)."]
			#[unsafe(no_mangle)]
			pub unsafe extern "C" fn $name(
				project: *const ProjectHandle,
				id: u64,
			) -> QuercusRational {
				let null = QuercusRational { num: 0, den: 0 };
				let Some(id) = node_id(id) else { return null };
				with_project(project, |p| {
					block_core(p, id).map(|$c| QuercusRational::from_native(&$body))
				})
				.flatten()
				.unwrap_or(null)
			}
		)*
	};
}

block_rational! {
	quercus_block_in => |c| c.in_(),
	quercus_block_out => |c| c.out(),
	quercus_block_length => |c| c.length(),
	quercus_block_media_in => |c| c.media_in,
	quercus_block_media_out => |c| c.media_out(),
}

/// The track's display height in pixels.
///
/// Converts the stored internal height to pixels (rounded) and floors it at 24 px,
/// matching the timeline's minimum grab height. The default track stores 3.0
/// internal units, which is 39 px.
///
/// # Returns
///
/// The height in pixels, or `64.0` (the default row height) for a null handle, a
/// malformed/stale id, or a non-track node. Pair with `quercus_track_height`, which
/// returns the raw internal units.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_height_pixels(
	project: *const ProjectHandle,
	id: u64,
) -> f64 {
	let Some(id) = node_id(id) else { return 64.0 };
	with_project(project, |p| {
		behavior_as::<TrackBehavior>(p, id)
			.map(|t| {
				f64::from(
					quercus_node::track::internal_height_to_pixel_height(t.height).max(24),
				)
			})
			.unwrap_or(64.0)
	})
	.unwrap_or(64.0)
}

/// The token of the track list that owns this track.
///
/// # Returns
///
/// The track-list token, or `QUERCUS_NODE_ID_INVALID` for a null handle, a
/// malformed/stale id, a non-track node, or a track that is not attached to a
/// list. Pair with `quercus_track_index` for the track's position in that list.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_list(project: *const ProjectHandle, id: u64) -> u64 {
	let Some(id) = node_id(id) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	with_project(project, |p| {
		behavior_as::<TrackBehavior>(p, id)
			.and_then(|t| t.track_list)
			.map(|l| l.identity())
			.unwrap_or(QUERCUS_NODE_ID_INVALID)
	})
	.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// The track's index inside its owning track list (0-based).
///
/// The index is stored on the track and maintained by the track-list editing
/// operations; this getter does not recompute it. Pair with `quercus_track_list`.
///
/// # Returns
///
/// The stored index, or `-1` for a null handle, a malformed/stale id, or a
/// non-track node.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_index(project: *const ProjectHandle, id: u64) -> i32 {
	let Some(id) = node_id(id) else { return -1 };
	with_project(project, |p| behavior_as::<TrackBehavior>(p, id).map(|t| t.index))
		.flatten()
		.unwrap_or(-1)
}

/// Creates a generator node from the node factory and adds it to the graph.
///
/// `type_id` is the factory's type id string (for example
/// `"org.olivevideoeditor.Olive.solidgenerator"`), not a display name. The type
/// must exist in the factory and be registered in the Generator category. The node
/// is added to the graph but not mounted in any folder, and the call is not
/// undoable, so the caller decides where it goes.
///
/// # Returns
///
/// The new node token, or `QUERCUS_NODE_ID_INVALID` for a null handle, a
/// null/non-UTF-8 `type_id`, an unknown type id, or a type that is not a
/// generator.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `type_id` must be
/// null or point to `type_id_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_generator_create(
	project: *const ProjectHandle,
	type_id: *const u8,
	type_id_len: usize,
) -> u64 {
	let Some(type_id) = (unsafe { str_arg(type_id, type_id_len) }) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	with_project_mut(project, |g| {
		let Some((core, behavior)) = quercus_node::factory::Factory::global().create_any(type_id)
		else {
			return QUERCUS_NODE_ID_INVALID;
		};
		if !behavior.categories().contains(&quercus_node::node::Category::Generator) {
			return QUERCUS_NODE_ID_INVALID;
		}
		g.graph.add_node(core, behavior).identity()
	})
	.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// Removes a project setting key from the settings map.
///
/// `key` is the setting name to delete; removing a key that is not present
/// mutates nothing, it simply reports `false`. A successful removal marks the
/// project modified.
///
/// # Returns
///
/// `false` for a null handle, a null/non-UTF-8 key, an empty key, or a key that
/// was not set; `true` when a key was actually removed.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `key` must be null
/// or point to `key_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_unset_setting(
	project: *const ProjectHandle,
	key: *const u8,
	key_len: usize,
) -> bool {
	let Some(key) = (unsafe { str_arg(key, key_len) }) else {
		return false;
	};
	if key.is_empty() {
		return false;
	}
	with_project_mut(project, |g| {
		let removed = g.settings.remove(key).is_some();
		if removed {
			g.set_modified(true);
		}
		removed
	})
	.unwrap_or(false)
}

/// Converts a pixel track height to the graph's internal height units.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_track_internal_height_from_pixels(pixels: i32) -> f64 {
	quercus_node::track::pixel_height_to_internal_height(pixels)
}

/// Reads the graph's virtual input and output endpoint tokens.
///
/// The endpoints are the well-known source/sink nodes the render graph is built
/// around. Both out pointers must be non-null; on success both `*input` and
/// `*output` are written. Nothing is written on failure, so callers can
/// initialise both slots to the invalid token and test the return value.
///
/// # Returns
///
/// `false` when either out pointer is null, the handle is null, or the project has
/// no graph endpoints (for example a project that was never initialized).
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `input` and
/// `output` must each be null or writable for one `u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_graph_endpoints(
	project: *const ProjectHandle,
	input: *mut u64,
	output: *mut u64,
) -> bool {
	if input.is_null() || output.is_null() {
		return false;
	}
	with_project(project, |p| p.graph.endpoints())
		.flatten()
		.map(|(i, o)| {
			unsafe {
				*input = i.identity();
				*output = o.identity();
			}
			true
		})
		.unwrap_or(false)
}

/// The token of the track that owns this block.
///
/// # Returns
///
/// The track token, or `QUERCUS_NODE_ID_INVALID` for a null handle, a malformed
/// id, a non-block node (a block is a clip, gap, transition or adjustment), or a
/// block that is not placed on any track.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_block_track(project: *const ProjectHandle, id: u64) -> u64 {
	let Some(id) = node_id(id) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	with_project(project, |p| {
		block_core(p, id)
			.and_then(|c| c.track)
			.map(|t| t.identity())
			.unwrap_or(QUERCUS_NODE_ID_INVALID)
	})
	.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// The block's playback speed multiplier (`1.0` = normal speed).
///
/// Stored on the block core and persisted with the block as its `speed`
/// element, so it survives save/load and copy/paste.
///
/// # Returns
///
/// The speed, or `1.0` for a null handle, a malformed/stale id, a non-block node,
/// or a block whose speed was never changed.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_block_speed(project: *const ProjectHandle, id: u64) -> f64 {
	let Some(id) = node_id(id) else { return 1.0 };
	with_project(project, |p| block_core(p, id).map(|c| c.speed))
		.flatten()
		.unwrap_or(1.0)
}

/// Number of blocks linked to this block (the linked A/V group).
///
/// Links are the associations that keep a clip's audio and video halves — and
/// other grouped blocks — edited together.
///
/// # Returns
///
/// The link count, or `0` for a null handle, a malformed id, or a non-block node.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_block_link_count(project: *const ProjectHandle, id: u64) -> i32 {
	let Some(id) = node_id(id) else { return 0 };
	with_project(project, |p| {
		block_core(p, id).map(|c| c.links.len() as i32)
	})
	.flatten()
	.unwrap_or(0)
}

/// One block token linked to this block (the linked A/V group).
///
/// `index` is a 0-based position in the block's link list. Pair with
/// `quercus_block_link_count` to iterate the group.
///
/// # Returns
///
/// The linked block token, or `QUERCUS_NODE_ID_INVALID` for a negative or
/// out-of-range `index`, a null handle, a malformed id, or a non-block node.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_block_link_at(
	project: *const ProjectHandle,
	id: u64,
	index: i32,
) -> u64 {
	let Some(id) = node_id(id) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	if index < 0 {
		return QUERCUS_NODE_ID_INVALID;
	}
	with_project(project, |p| {
		block_core(p, id)
			.and_then(|c| c.links.get(index as usize).copied())
			.map(|l| l.identity())
			.unwrap_or(QUERCUS_NODE_ID_INVALID)
	})
	.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// The node's color override as a palette index.
///
/// `-1` means no override, which is what a freshly created node has. The value is
/// persisted in the project XML as the node's `color` element and survives
/// `quercus_project_save`/`quercus_project_load`; it is the bin/timeline color
/// label, not a render color.
///
/// # Returns
///
/// The override index, or `-1` for a null handle, a malformed/stale id, or a node
/// without an override.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_node_color(project: *const ProjectHandle, id: u64) -> i32 {
	let Some(id) = node_id(id) else { return -1 };
	with_project(project, |p| p.graph.get(id).map(|e| e.core.override_color))
		.flatten()
		.unwrap_or(-1)
}

/// The footage node connected to a clip block.
///
/// # Returns
///
/// The footage token when `id` is a clip block with footage assigned, and
/// `QUERCUS_NODE_ID_INVALID` for a null handle, a malformed id, a non-clip node,
/// or a clip that has no footage set. Gaps, transitions and adjustments always
/// fail here because they never carry footage.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_block_footage(project: *const ProjectHandle, id: u64) -> u64 {
	let Some(id) = node_id(id) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	with_project(project, |p| {
		behavior_as::<ClipBlockBehavior>(p, id)
			.and_then(|c| c.footage)
			.map(|f| f.identity())
			.unwrap_or(QUERCUS_NODE_ID_INVALID)
	})
	.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// The multicam node feeding a block's texture input, when there is one.
///
/// Looks at the block's `tex_in` connection (element `-1`) and returns the source
/// token only if that source's type id is the multicam node type.
///
/// # Returns
///
/// The multicam source token, or `QUERCUS_NODE_ID_INVALID` for a null handle, a
/// malformed `id`, a block with nothing connected to `tex_in`, or a source that is
/// not a multicam node.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_block_multicam_source(
	project: *const ProjectHandle,
	id: u64,
) -> u64 {
	let Some(id) = node_id(id) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	with_project(project, |p| {
		p.graph
			.connected_output(id, quercus_node::block::clip_input::TEXTURE_INPUT, -1)
			.filter(|&source| {
				p.graph
					.get(source)
					.is_some_and(|e| e.behavior.type_id() == "org.olivevideoeditor.Olive.multicam")
			})
			.map(|s| s.identity())
			.unwrap_or(QUERCUS_NODE_ID_INVALID)
	})
	.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

/// Width in seconds of the transition attached to one edge of a block.
///
/// For `start_edge == true` the function looks at the previous block on the same
/// track and uses its `out_offset` when that block is a transition whose
/// `in_block_in` input is connected to this block; for `start_edge == false` it
/// looks at the next block and uses that transition's `in_offset` when its
/// `out_block_in` input is connected to this block. Timeline snapshots use this to
/// draw the transition handles on the clip edge.
///
/// # Arguments
///
/// * `id` - the clip/gap block token whose edge is queried.
/// * `start_edge` - `true` for the transition entering the block, `false` for the
///   transition leaving it.
///
/// # Returns
///
/// The transition offset as a rational in seconds, or the null rational `0/0` when
/// the edge carries no transition, the block is not on a track, the handle is
/// null, or `id` is not a block.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_block_transition_width(
	project: *const ProjectHandle,
	id: u64,
	start_edge: bool,
) -> QuercusRational {
	let null = QuercusRational { num: 0, den: 0 };
	let Some(id) = node_id(id) else { return null };
	with_project(project, |p| {
		let core = block_core(p, id)?;
		let track = core.track?;
		let blocks = &behavior_as::<TrackBehavior>(p, track)?.blocks;
		let index = blocks.iter().position(|&b| b == id)?;
		let candidate = if start_edge {
			blocks.get(index.checked_sub(1)?).copied()?
		} else {
			blocks.get(index + 1).copied()?
		};
		let input = if start_edge {
			quercus_node::block::transition_input::IN_BLOCK
		} else {
			quercus_node::block::transition_input::OUT_BLOCK
		};
		if p.graph.connected_output(candidate, input, -1) != Some(id) {
			return None;
		}
		let transition = behavior_as::<TransitionBlockBehavior>(p, candidate)?;
		Some(QuercusRational::from_native(if start_edge {
			&transition.out_offset
		} else {
			&transition.in_offset
		}))
	})
	.flatten()
	.unwrap_or(null)
}

/// Number of live nodes in the project graph.
///
/// Counts every node in the graph arena, regardless of kind and regardless of
/// whether it is mounted in the bin. Pair with `quercus_project_node_at` to
/// enumerate them.
///
/// # Returns
///
/// `0` for a null handle.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_node_count(project: *const ProjectHandle) -> i32 {
	with_project(project, |p| p.graph.node_ids().len() as i32).unwrap_or(0)
}

/// The `index`-th live node token, in graph arena order.
///
/// `index` is a 0-based position in the graph arena; deleted nodes are skipped,
/// but the remaining order is graph insertion order, not bin or timeline order.
/// Use the folder/track traversal getters when structural order matters.
///
/// # Returns
///
/// `QUERCUS_NODE_ID_INVALID` for a negative or out-of-range `index`, or for a
/// null handle.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_node_at(
	project: *const ProjectHandle,
	index: i32,
) -> u64 {
	if index < 0 {
		return QUERCUS_NODE_ID_INVALID;
	}
	with_project(project, |p| {
		p.graph
			.node_ids()
			.get(index as usize)
			.map(|id| id.identity())
			.unwrap_or(QUERCUS_NODE_ID_INVALID)
	})
	.unwrap_or(QUERCUS_NODE_ID_INVALID)
}

// ---------------------------------------------------------------------------
// Node parameter reads (QuercusNodeValue POD mirror)
// ---------------------------------------------------------------------------

/// Reads an input's static (non-animated) value into `*out`.
///
/// `input_id` names the declared input (for example `"media_in_in"` on a clip)
/// and `element` is the array element index or `-1` for a scalar input. `*out`
/// receives a POD-mirror `QuercusNodeValue` tagged with the input's declared type.
/// Keyframes are ignored here: a keyframed input still returns its static value,
/// so use `quercus_node_value_at_time` to evaluate animation.
///
/// # Returns
///
/// `false` — leaving `*out` untouched — for a null `out` or handle, a
/// malformed/stale `id`, a null/non-UTF-8 `input_id`, an unknown input, or a
/// declared type with no POD representation (strings are not bridged yet).
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `input_id` must
/// be null or point to `input_id_len` readable bytes of valid UTF-8, and `out`
/// must be null or writable for one `QuercusNodeValue`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_node_standard_value(
	project: *const ProjectHandle,
	id: u64,
	input_id: *const u8,
	input_id_len: usize,
	element: i32,
	out: *mut quercus_node::value::QuercusNodeValue,
) -> bool {
	read_value(project, id, input_id, input_id_len, element, None, out)
}

/// Reads an input's value at a given time, interpolating keyframes when the input
/// is animated.
///
/// Uses the same arguments as `quercus_node_standard_value`: `input_id` names the
/// declared input and `element` is the array element index or `-1` for a scalar
/// input. `time` is a rational time in seconds; when the (input, element)
/// keyframe track is non-empty the value is interpolated at that time, otherwise
/// the static value is returned.
///
/// # Returns
///
/// `false` under the same conditions as `quercus_node_standard_value`, including
/// string-typed inputs; on `false`, `*out` is left untouched.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `input_id` must
/// be null or point to `input_id_len` readable bytes of valid UTF-8, and `out`
/// must be null or writable for one `QuercusNodeValue`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_node_value_at_time(
	project: *const ProjectHandle,
	id: u64,
	input_id: *const u8,
	input_id_len: usize,
	element: i32,
	time: QuercusRational,
	out: *mut quercus_node::value::QuercusNodeValue,
) -> bool {
	read_value(
		project,
		id,
		input_id,
		input_id_len,
		element,
		Some(time.to_native()),
		out,
	)
}

/// Shared implementation of the two parameter-read entry points.
///
/// Resolves the node and the input's declared type, reads either the static value
/// (`time == None`) or the keyframe-interpolated value at `time`, and converts the
/// result to the ABI `QuercusNodeValue`. `element` is the array element index or
/// `-1` for a scalar input. `*out` is written only on success, so a `false`
/// return leaves the caller's value untouched.
///
/// # Returns
///
/// `false` for a null `out`, a null handle, a malformed/stale `token`, a
/// null/non-UTF-8 `input_id`, an unknown input, or a declared type without a POD
/// representation (strings).
fn read_value(
	project: *const ProjectHandle,
	token: u64,
	input_id: *const u8,
	input_id_len: usize,
	element: i32,
	time: Option<quercus_core::Rational>,
	out: *mut quercus_node::value::QuercusNodeValue,
) -> bool {
	if out.is_null() {
		return false;
	}
	let Some(id) = node_id(token) else { return false };
	let Some(input) = (unsafe { str_arg(input_id, input_id_len) }) else {
		return false;
	};
	let value = with_project(project, |p| {
		let entry = p.graph.get(id)?;
		let declared = entry.core.input_data_type(input)?;
		let v = match time {
			Some(t) => entry.core.value_at_time(input, element, t),
			None => entry.core.standard_value(input, element),
		};
		quercus_node::value::QuercusNodeValue::from_node_value(declared, &v).ok()
	})
	.flatten();
	match value {
		Some(v) => {
			unsafe { *out = v };
			true
		}
		None => false,
	}
}

// ---------------------------------------------------------------------------
// Undo stack (read-only)
// ---------------------------------------------------------------------------

/// The node's user label, snprintf-style.
///
/// Follows the crate's buffer convention: the full byte length excluding the NUL
/// is returned, `min(len, buf_len - 1)` bytes plus a NUL are written, a null
/// `buf` (or `buf_len == 0`) is a size query, and `ret >= buf_len` means the
/// output was truncated. An unlabeled node yields an empty string (a return
/// length of `0`), not `-1`. Pair with `quercus_node_set_label`.
///
/// # Returns
///
/// `-1` for a null handle or a malformed/unknown `id`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `buf` must be
/// null or writable for `buf_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_node_label(
	project: *const ProjectHandle,
	id: u64,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let Some(id) = NodeId::from_identity(id) else {
		return -1;
	};
	match with_project(project, |g| g.graph.get(id).map(|e| e.core.label.clone())).flatten() {
		Some(label) => copy_str_out(&label, buf, buf_len),
		None => -1,
	}
}

/// Sets the node's user label (the display name shown in the bin and timeline).
///
/// This is a direct graph write matching `quercus_task::nodeops::set_node_label`:
/// it is not an undoable edit and does not set the project's modified flag. An
/// empty label is accepted and clears the visible name. Pair with
/// `quercus_node_label`.
///
/// # Returns
///
/// `false` for a null handle, a malformed `id` token, or a null/non-UTF-8
/// `label`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `label` must be
/// null or point to `label_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_node_set_label(
	project: *const ProjectHandle,
	id: u64,
	label: *const u8,
	label_len: usize,
) -> bool {
	let Some(id) = NodeId::from_identity(id) else {
		return false;
	};
	let Some(label) = (unsafe { str_arg(label, label_len) }) else {
		return false;
	};
	with_project_mut(project, |g| {
		let Some(entry) = g.graph.get_mut(id) else {
			return false;
		};
		entry.core.label = label.to_string();
		true
	})
	.unwrap_or(false)
}

/// The sequence's length in rational seconds.
///
/// Returns the computed end of the sequence's mounted content (the largest
/// block out-point across its tracks) when that is greater than zero, otherwise
/// the sequence's cached `last_length`. A null handle or non-sequence token
/// returns the null rational `0/0`; a valid but empty sequence returns a
/// reduced zero such as `0/1`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_sequence_length(
	project: *const ProjectHandle,
	id: u64,
) -> QuercusRational {
	let Some(id) = NodeId::from_identity(id) else {
		return QuercusRational::default();
	};
	with_project(project, |g| {
		g.graph
			.get(id)
			.and_then(|e| e.behavior.as_any())
			.and_then(|a| a.downcast_ref::<SequenceBehavior>())
			.map(|s| {
				let content = crate::editor_commands::sequence::sequence_length(&g.graph, id);
				let length = if content > Rational::new(0, 1) {
					content
				} else {
					s.last_length
				};
				QuercusRational::from_native(&length)
			})
	})
	.flatten()
	.unwrap_or_default()
}

/// The sequence's frame rate as a rational in frames per second.
///
/// Read from the sequence's first video stream. Returns the null rational `0/0`
/// for a null handle, a malformed/non-sequence id, or a sequence with no video
/// stream parameters, so callers must check `den != 0` before dividing. Pair with
/// `quercus_sequence_geometry` and `quercus_sequence_length`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_sequence_frame_rate(
	project: *const ProjectHandle,
	id: u64,
) -> QuercusRational {
	let Some(id) = NodeId::from_identity(id) else {
		return QuercusRational::default();
	};
	with_project(project, |g| {
		g.graph
			.get(id)
			.and_then(|e| e.behavior.as_any())
			.and_then(|a| a.downcast_ref::<SequenceBehavior>())
			.and_then(|s| s.video_params.first())
			.map(|v| QuercusRational::from_native(&v.frame_rate))
	})
	.flatten()
	.unwrap_or_default()
}

/// Reads the sequence's output width and height in pixels.
///
/// Both out pointers are optional: a null `width` or `height` is simply not
/// written. The values come from the sequence's first video stream, so a sequence
/// with no video parameters fails instead of reporting 0x0. Pair with
/// `quercus_sequence_set_video_params`.
///
/// # Returns
///
/// `false` — writing neither output — for a null handle, a non-sequence id, or a
/// sequence with no video stream parameters; `true` otherwise.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `width` and
/// `height` must each be null or writable for one `i32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_sequence_geometry(
	project: *const ProjectHandle,
	id: u64,
	width: *mut i32,
	height: *mut i32,
) -> bool {
	let Some(id) = NodeId::from_identity(id) else {
		return false;
	};
	let params = with_project(project, |g| {
		g.graph
			.get(id)
			.and_then(|e| e.behavior.as_any())
			.and_then(|a| a.downcast_ref::<SequenceBehavior>())
			.and_then(|s| s.video_params.first())
			.map(|v| (v.width, v.height))
	})
	.flatten();
	let Some((w, h)) = params else {
		return false;
	};
	if !width.is_null() {
		unsafe { *width = w };
	}
	if !height.is_null() {
		unsafe { *height = h };
	}
	true
}

/// Sets the sequence's output geometry and frame rate.
///
/// `width`/`height` are pixels and `frame_rate_num`/`frame_rate_den` form the
/// rational frame rate (for example `30000`/`1001` for 29.97 fps). When the
/// sequence has no video stream yet, one is created with F32 pixel format, four
/// channels and interlace off; otherwise the first stream's width, height and
/// frame rate are overwritten and its remaining fields are kept. A zero
/// `frame_rate_den` produces the null rational `0/0` instead of an error. This is
/// a direct graph write: not undoable, and it does not set the project's modified
/// flag.
///
/// # Returns
///
/// `false` for a null handle or a malformed/non-sequence `id`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_sequence_set_video_params(
	project: *const ProjectHandle,
	id: u64,
	width: i32,
	height: i32,
	frame_rate_num: i32,
	frame_rate_den: i32,
) -> bool {
	let Some(id) = NodeId::from_identity(id) else {
		return false;
	};
	with_project_mut(project, |g| {
		let Some(s) = g
			.graph
			.get_mut(id)
			.and_then(|e| e.behavior.as_any_mut())
			.and_then(|a| a.downcast_mut::<SequenceBehavior>())
		else {
			return false;
		};
		if s.video_params.is_empty() {
			s.video_params.push(quercus_node::value::VideoParams {
				width,
				height,
				frame_rate: quercus_core::Rational::new(
					i64::from(frame_rate_num),
					i64::from(frame_rate_den),
				),
				pixel_format: quercus_core::PixelFormat::F32 as i32,
				channels: 4,
				interlaced: false,
			});
		} else {
			let v = &mut s.video_params[0];
			v.width = width;
			v.height = height;
			v.frame_rate = quercus_core::Rational::new(
				i64::from(frame_rate_num),
				i64::from(frame_rate_den),
			);
		}
		true
	})
	.unwrap_or(false)
}

/// Sets the interlaced flag on the sequence's first video stream.
///
/// Fails for a null handle or a non-sequence token. For a sequence that has no
/// video stream the flag cannot be stored, so the call changes nothing but still
/// returns `true` (the sequence was found). Pair with
/// `quercus_sequence_interlaced`.
///
/// # Returns
///
/// `false` for a null handle or a malformed/non-sequence `id`; `true` otherwise.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_sequence_set_interlaced(
	project: *const ProjectHandle,
	id: u64,
	interlaced: bool,
) -> bool {
	let Some(id) = NodeId::from_identity(id) else {
		return false;
	};
	with_project_mut(project, |g| {
		let Some(s) = g
			.graph
			.get_mut(id)
			.and_then(|e| e.behavior.as_any_mut())
			.and_then(|a| a.downcast_mut::<SequenceBehavior>())
		else {
			return false;
		};
		if let Some(v) = s.video_params.first_mut() {
			v.interlaced = interlaced;
		}
		true
	})
	.unwrap_or(false)
}

/// Whether the process-wide undo stack can undo right now.
///
/// The stack is global — shared by all projects and by this crate's editing entry
/// points — so the answer is not scoped to a project. Pair with
/// `quercus_undo_undo` and `quercus_undo_can_redo`.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_undo_can_undo() -> bool {
	quercus_undo::global::can_undo()
}

/// Whether the global undo stack can redo.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_undo_can_redo() -> bool {
	quercus_undo::global::can_redo()
}

/// Number of commands on the undo stack (-1 on error).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_undo_count() -> i64 {
	quercus_undo::global::count().unwrap_or(-1)
}

/// The current position on the undo stack (-1 on error).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_undo_index() -> i64 {
	quercus_undo::global::index().unwrap_or(-1)
}

/// Serializes every test that touches the process-global undo stack
/// (all three test modules in this file share it).
#[cfg(test)]
pub(crate) static UNDO_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
	use super::*;

	fn cstr_arg(s: &str) -> (*const u8, usize) {
		(s.as_ptr(), s.len())
	}

	/// A project with one footage node in the root folder.
	fn project_with_footage() -> *mut ProjectHandle {
		let project = quercus_project_new();
		assert!(!project.is_null());
		// Build a footage node through quercus-node directly (the engine's
		// read API is what's under test).
		{
			let inner = unsafe { &*project }.inner.clone();
			let mut g = inner.lock().unwrap();
			let (core, mut behavior) = quercus_node::footage::FootageBehavior::create();
			{
				let b = behavior
					.as_any_mut()
					.unwrap()
					.downcast_mut::<FootageBehavior>()
					.unwrap();
				b.filename = "/tmp/quercus-node-test.mp4".to_string();
			}
			let id = g.graph.add_node(core, behavior);
			let root = g.root;
			g.graph
				.get_mut(root)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<FolderBehavior>()
				.unwrap()
				.add_child(id);
		}
		project
	}

	#[test]
	fn id_tokens_and_project_basics() {
		assert_eq!(quercus_node_id_invalid(), u32::MAX as u64);
		assert!(!quercus_node_id_is_valid(quercus_node_id_invalid()));

		let project = quercus_project_new();
		unsafe {
			let root = quercus_project_root(project);
			assert!(quercus_node_id_is_valid(root), "initialized project has a root");
			assert_eq!(quercus_node_kind(project, root), 1, "root is a folder");
			assert_eq!(quercus_folder_child_count(project, root), 0);

			let mut buf = [0u8; 128];
			let n = quercus_node_type_id(project, root, buf.as_mut_ptr(), buf.len());
			assert!(std::str::from_utf8(&buf[..n as usize]).unwrap().ends_with("folder"));
			assert!(quercus_project_filename(project, buf.as_mut_ptr(), buf.len()) >= 0);
			assert!(!quercus_project_modified(project));

			quercus_project_release(project);
		}
	}

	/// Node labels and sequence introspection through the C ABI.
	#[test]
	fn node_label_and_sequence_params() {
		// Folder creation is undoable; serialize against the other
		// undo-stack tests.
		let _guard = UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		unsafe {
			let project = quercus_project_new();
			let root = quercus_project_root(project);
			let (n, nl) = ("Labeled".as_ptr(), 7);
			let folder = quercus_folder_create(project, root, n, nl);
			assert!(quercus_node_id_is_valid(folder));

			let mut buf = [0u8; 64];
			let len = quercus_node_label(project, folder, buf.as_mut_ptr(), buf.len());
			assert_eq!(std::str::from_utf8(&buf[..len as usize]).unwrap(), "Labeled");
			assert!(quercus_node_set_label(project, folder, "Renamed".as_ptr(), 7));
			let len = quercus_node_label(project, folder, buf.as_mut_ptr(), buf.len());
			assert_eq!(std::str::from_utf8(&buf[..len as usize]).unwrap(), "Renamed");
			assert_eq!(quercus_node_label(project, u64::MAX, buf.as_mut_ptr(), 64), -1);
			assert!(!quercus_node_set_label(project, u64::MAX, "x".as_ptr(), 1));

			let (s, sl) = ("Seq".as_ptr(), 3);
			let seq = quercus_sequence_create(project, s, sl);
			// A fresh sequence carries the default 29.97 fps video params.
			assert_eq!(quercus_sequence_frame_rate(project, seq), QuercusRational { num: 30000, den: 1001 });
			assert!(quercus_sequence_geometry(project, seq, std::ptr::null_mut(), std::ptr::null_mut()));
			assert!(quercus_sequence_set_video_params(project, seq, 1920, 1080, 24, 1));
			assert_eq!(quercus_sequence_frame_rate(project, seq), QuercusRational { num: 24, den: 1 });
			let (mut w, mut h) = (0, 0);
			assert!(quercus_sequence_geometry(project, seq, &mut w, &mut h));
			assert_eq!((w, h), (1920, 1080));
			assert_eq!(quercus_sequence_length(project, seq), QuercusRational { num: 0, den: 1 });
			assert!(!quercus_sequence_set_video_params(project, u64::MAX, 1, 1, 1, 1));

			quercus_project_release(project);
			assert!(quercus_undo::global::clear().is_ok());
		}
	}

	#[test]
	fn active_project_slot() {
		unsafe {
			quercus_project_set_active(std::ptr::null());
			assert!(quercus_project_active().is_null());

			let project = quercus_project_new();
			quercus_project_set_active(project);
			let active = quercus_project_active();
			assert!(!active.is_null());
			// Same underlying project: the root token matches.
			assert_eq!(quercus_project_root(active), quercus_project_root(project));

			quercus_project_set_active(std::ptr::null());
			assert!(quercus_project_active().is_null());

			quercus_project_release(active);
			quercus_project_release(project);
		}
	}

	#[test]
	fn footage_traversal() {
		let project = project_with_footage();
		unsafe {
			let root = quercus_project_root(project);
			assert_eq!(quercus_folder_child_count(project, root), 1);
			let child = quercus_folder_child_at(project, root, 0);
			assert!(quercus_node_id_is_valid(child));
			assert_eq!(quercus_node_kind(project, child), 2, "footage");

			let mut buf = [0u8; 128];
			let n = quercus_footage_filename(project, child, buf.as_mut_ptr(), buf.len());
			assert_eq!(
				std::str::from_utf8(&buf[..n as usize]).unwrap(),
				"/tmp/quercus-node-test.mp4"
			);

			// Unprobed: zero streams.
			assert_eq!(quercus_footage_total_stream_count(project, child), 0);
			assert!(!quercus_footage_video_params(project, child, 0, std::ptr::null_mut()));

			quercus_project_release(project);
		}
	}

	#[test]
	fn sequence_track_tracklist_traversal() {
		let project = quercus_project_new();
		{
			let inner = unsafe { &*project }.inner.clone();
			let mut g = inner.lock().unwrap();
			// sequence → tracklist(video) → track(video)
			let (seq_core, seq_b) = quercus_node::sequence::SequenceBehavior::create();
			let seq = g.graph.add_node(seq_core, seq_b);
			let (tl_core, tl_b) = quercus_node::track::TrackListBehavior::create();
			let tl = g.graph.add_node(tl_core, tl_b);
			let (tr_core, tr_b) = quercus_node::track::TrackBehavior::create();
			let tr = g.graph.add_node(tr_core, tr_b);

			let tl_b = g
				.graph
				.get_mut(tl)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackListBehavior>()
				.unwrap();
			tl_b.tracks.push(tr);
			let seq_b = g
				.graph
				.get_mut(seq)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<SequenceBehavior>()
				.unwrap();
			seq_b.track_lists.push(tl);
		}
		unsafe {
			let root = quercus_project_root(project);
			let _ = root;
			// find the sequence via graph scan (it is not in the bin)
			// — simpler: the traversal entry points are exercised directly.
			// (sequence id is not discoverable via the folder tree)
			let mut found_seq = 0u64;
			{
				let inner = &*project;
				let g = inner.inner.lock().unwrap();
				for id in g.graph.node_ids() {
					if g.graph.get(id).unwrap().behavior.type_id()
						== "org.olivevideoeditor.Olive.sequence"
					{
						found_seq = id.identity();
					}
				}
			}
			assert_eq!(quercus_sequence_track_list_count(project, found_seq), 1);
			let tl = quercus_sequence_track_list_at(project, found_seq, 0);
			assert_eq!(quercus_tracklist_track_count(project, tl), 1);
			let tr = quercus_tracklist_track_at(project, tl, 0);
			assert_eq!(quercus_track_block_count(project, tr), 0);
			quercus_project_release(project);
		}
	}

	#[test]
	fn param_reads_and_undo_readonly() {
		let project = project_with_footage();
		unsafe {
			let root = quercus_project_root(project);
			let child = quercus_folder_child_at(project, root, 0);

			// footage has a "file_in" text input — not POD-representable,
			// so the read must fail cleanly (false), not crash.
			let mut out = quercus_node::value::QuercusNodeValue::none();
			let (i, il) = cstr_arg("file_in");
			assert!(!quercus_node_standard_value(project, child, i, il, -1, &mut out));
			// Unknown input id: false.
			let (i, il) = cstr_arg("no_such_input");
			assert!(!quercus_node_standard_value(project, child, i, il, -1, &mut out));
			// value_at_time on a static text input: also false (string).
			let (i, il) = cstr_arg("file_in");
			assert!(!quercus_node_value_at_time(
				project,
				child,
				i,
				il,
				-1,
				QuercusRational { num: 0, den: 1 },
				&mut out,
			));

			// Fresh process-wide stack state is whatever the suite left;
			// just verify the calls work.
			let _ = quercus_undo_can_undo();
			let _ = quercus_undo_can_redo();
			assert!(quercus_undo_count() >= 0);
			assert!(quercus_undo_index() >= 0 || quercus_undo_count() == 0);

			quercus_project_release(project);
		}
	}

	#[test]
	fn null_safety_sweep() {
		unsafe {
			let mut buf = [0u8; 64];
			assert_eq!(quercus_node_kind(std::ptr::null(), 1), 0);
			assert_eq!(quercus_node_type_id(std::ptr::null(), 1, buf.as_mut_ptr(), 64), -1);
			assert_eq!(quercus_folder_child_count(std::ptr::null(), 1), 0);
			assert_eq!(quercus_folder_child_at(std::ptr::null(), 1, 0), QUERCUS_NODE_ID_INVALID);
			assert_eq!(quercus_footage_filename(std::ptr::null(), 1, buf.as_mut_ptr(), 64), -1);
			assert_eq!(quercus_footage_duration(std::ptr::null(), 1).den, 0);
			assert_eq!(quercus_sequence_track_list_count(std::ptr::null(), 1), 0);
			assert_eq!(quercus_tracklist_kind(std::ptr::null(), 1), -1);
			assert_eq!(quercus_track_length(std::ptr::null(), 1).num, 0);
			assert_eq!(quercus_block_in(std::ptr::null(), 1).den, 0);
			assert!(!quercus_project_modified(std::ptr::null()));
			assert_eq!(quercus_project_root(std::ptr::null()), QUERCUS_NODE_ID_INVALID);
			quercus_project_release(std::ptr::null());
		}
	}
}

// ---------------------------------------------------------------------------
// Batch 2: editing (undo-integrated)
// ---------------------------------------------------------------------------
//
// Every mutation pushes an quercus-timeline/quercus-task undo command through the
// global undo stack, exactly like the app's own edits. Plugins batch
// several calls into one undo step with quercus_undo_group_begin/end.

use quercus_node::value::NodeValue;
use quercus_timeline::util::NodeRef;
use quercus_undo::undocommand::UndoCommand;

/// Runs `f` with mutable access to the project behind `project`, holding the
/// project mutex for the whole call.
///
/// Returns `None` without calling `f` when `project` is null. Poisoned locks are
/// recovered with `into_inner`, so a panic on another thread does not make the
/// editing entry points panic here. `f` runs under the lock and must not re-enter
/// another entry point that locks the same project (that would deadlock).
fn with_project_mut<R>(project: *const ProjectHandle, f: impl FnOnce(&mut Project) -> R) -> Option<R> {
	if project.is_null() {
		return None;
	}
	let mut guard = unsafe { &*project }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	Some(f(&mut guard))
}

/// Pushes `cmd` onto the process-wide undo stack under `name` and reports whether
/// the push succeeded.
///
/// The stack's push path executes the command (redo) before recording it, so a
/// successful call both applies the edit and makes it undoable; callers in this
/// module turn a `false` into their own failure return. `name` is the label the
/// host shows for that undo entry.
fn push_undo(cmd: UndoCommand, name: &str) -> bool {
	quercus_undo::global::push(cmd, name).is_ok()
}

/// Opens an undo group: commands pushed until `quercus_undo_group_end` collapse
/// into one undo entry labelled `name`.
///
/// `name` is the label of the resulting step (for example `"Import Two Clips"`).
/// Groups do not nest: the call fails when a group is already open. Close the
/// group with `quercus_undo_group_end`, or discard its commands with
/// `quercus_undo_group_abort`.
///
/// # Returns
///
/// `false` for a null/non-UTF-8 `name` or when a group is already open.
///
/// # Safety
///
/// `name` must be null or point to `name_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_undo_group_begin(name: *const u8, name_len: usize) -> bool {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return false;
	};
	quercus_undo::global::group_begin(name).is_ok()
}

/// Closes the current undo group. False when none is open.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_undo_group_end() -> bool {
	quercus_undo::global::group_end().is_ok()
}

/// Aborts the current undo group, rolling it back. False when none is
/// open.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_undo_group_abort() -> bool {
	quercus_undo::global::group_abort().is_ok()
}

/// Undoes one step. False when the stack is empty.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_undo_undo() -> bool {
	quercus_undo::global::undo().is_ok()
}

/// Redoes one step. False when nothing to redo.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_undo_redo() -> bool {
	quercus_undo::global::redo().is_ok()
}

/// Imports a media file into the project's bin root and returns its footage
/// token.
///
/// `filename` is a UTF-8 path, absolute or relative to the process working
/// directory; the file must exist or the import fails before any node is created.
/// The new node stores the path as both its `file_in` value and its footage
/// filename, is labelled with the file name, and is probed *before* it enters the
/// graph so a failed probe leaves no orphan node and nothing on the undo stack.
/// The bin insertion is pushed as one undoable "Import Footage" step; if that
/// push fails, the node exists in the graph but is not in the bin and the call
/// reports failure.
///
/// # Returns
///
/// The footage token, or `QUERCUS_NODE_ID_INVALID` for a null handle, a
/// null/non-UTF-8 or non-existent path, an uninitialized project root, a failed
/// probe, or a failed undo push.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `filename` must
/// be null or point to `filename_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_footage_import(
	project: *const ProjectHandle,
	filename: *const u8,
	filename_len: usize,
) -> u64 {
	let Some(filename) = (unsafe { str_arg(filename, filename_len) }) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	if project.is_null() {
		return QUERCUS_NODE_ID_INVALID;
	}
	if !std::path::Path::new(filename).exists() {
		return QUERCUS_NODE_ID_INVALID;
	}
	let id = {
		let Some(id) = with_project_mut(project, |guard| {
			if !guard.root.valid() {
				return None;
			}
			let (mut core, mut behavior) = FootageBehavior::create();
			core.set_standard_value(
				"file_in",
				-1,
				NodeValue::Text(filename.to_string()),
			);
			let f = behavior
				.as_any_mut()
				.and_then(|a| a.downcast_mut::<FootageBehavior>())?;
			f.filename = filename.to_string();
			// Probe before the node enters the graph so a failed probe
			// leaves no orphan behind (and nothing on the undo stack).
			f.probe().ok()?;
			let id = guard.graph.add_node(core, behavior);
			if let Some(e) = guard.graph.get_mut(id) {
				e.core.label = std::path::Path::new(filename)
					.file_name()
					.map(|f| f.to_string_lossy().into_owned())
					.unwrap_or_else(|| filename.to_string());
			}
			Some(id)
		})
		.flatten() else {
			return QUERCUS_NODE_ID_INVALID;
		};
		id
	};
	let root = unsafe { &*project }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.root;
	let project_ref = unsafe { &*project }.inner.clone();
	if !push_undo(
		quercus_task::nodeops::folder_add_child_command(
			(project_ref.clone(), root),
			(project_ref, id),
		),
		"Import Footage",
	) {
		return QUERCUS_NODE_ID_INVALID;
	}
	id.identity()
}

/// Creates a folder under `parent` and returns its token.
///
/// `parent` is any folder token, including `quercus_project_root`; `name` becomes
/// both the folder behavior's name and the node's label, and an empty name is
/// accepted. The folder node is added to the graph and the bin insertion is
/// pushed as one undoable "Create Folder" step. If the undo push fails the call
/// reports failure after the node was already added, so a failed create can leave
/// an unparented folder in the graph.
///
/// # Returns
///
/// The new folder token, or `QUERCUS_NODE_ID_INVALID` for a null handle, a
/// malformed `parent` token, a null/non-UTF-8 `name`, or a failed undo push.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `name` must be
/// null or point to `name_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_folder_create(
	project: *const ProjectHandle,
	parent: u64,
	name: *const u8,
	name_len: usize,
) -> u64 {
	let Some(parent) = node_id(parent) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	if project.is_null() {
		return QUERCUS_NODE_ID_INVALID;
	}
	let id = with_project_mut(project, |guard| {
		let (mut core, behavior) = quercus_node::folder::create(name);
		core.label = name.to_string();
		guard.graph.add_node(core, behavior)
	});
	let Some(id) = id else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let project_ref = unsafe { &*project }.inner.clone();
	if !push_undo(
		quercus_task::nodeops::folder_add_child_command(
			(project_ref.clone(), parent),
			(project_ref, id),
		),
		"Create Folder",
	) {
		return QUERCUS_NODE_ID_INVALID;
	}
	id.identity()
}

/// Removes a node from the project graph as one undoable "Remove Node" step.
///
/// The removal is a `remove_node_command`; for a well-formed token that names no
/// live node the command is a no-op, so the return value reports only whether the
/// push succeeded. The removal is reversed by `quercus_undo_undo`.
///
/// # Returns
///
/// `false` for a null handle, a malformed token, or a failed undo push.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_node_remove(project: *const ProjectHandle, id: u64) -> bool {
	let Some(id) = node_id(id) else { return false };
	if project.is_null() {
		return false;
	}
	let project_ref = unsafe { &*project }.inner.clone();
	push_undo(quercus_task::nodeops::remove_node_command(project_ref, id), "Remove Node")
}

/// Creates a sequence, mounts it in the bin root, and gives it the default
/// two-video/two-audio track layout.
///
/// `name` is stored as the sequence's label; null or non-UTF-8 bytes fail.
/// Creation is deliberately not undoable, matching the application: the bin mount
/// and the four default tracks are applied directly rather than pushed onto the
/// undo stack. The bin mount is best-effort, so a project whose root is not
/// initialized still returns the sequence token, just unparented.
///
/// # Returns
///
/// The new sequence token, or `QUERCUS_NODE_ID_INVALID` for a null handle or a
/// null/non-UTF-8 `name`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `name` must be
/// null or point to `name_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_sequence_create(
	project: *const ProjectHandle,
	name: *const u8,
	name_len: usize,
) -> u64 {
	let Some(name) = (unsafe { str_arg(name, name_len) }) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	if project.is_null() {
		return QUERCUS_NODE_ID_INVALID;
	}
	let Some(seq) = with_project_mut(project, |guard| {
		let (mut core, behavior) = SequenceBehavior::create();
		core.label = name.to_string();
		guard.graph.add_node(core, behavior)
	}) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let project_ref = unsafe { &*project }.inner.clone();
	let root = {
		let guard = project_ref.lock().unwrap_or_else(|e| e.into_inner());
		guard.root
	};
	// Mount under the bin root. Not pushed: part of creation, like the app.
	quercus_task::nodeops::folder_add_child_command(
		(project_ref.clone(), root),
		(project_ref.clone(), seq),
	)
	.redo_now();
	// The default 2V+2A layout, also part of creation.
	for kind in [
		quercus_node::track::TrackType::Video,
		quercus_node::track::TrackType::Video,
		quercus_node::track::TrackType::Audio,
		quercus_node::track::TrackType::Audio,
	] {
		let Some(list) = find_or_create_track_list(&project_ref, seq, kind) else {
			continue;
		};
		quercus_timeline::undogeneral::TimelineAddTrackCommand::new(NodeRef::new(
			project_ref.clone(),
			list,
		))
		.redo();
	}
	seq.identity()
}

/// Returns the track list of `kind` owned by `seq`, creating and wiring one when
/// the sequence has none.
///
/// Takes the shared `Arc<Mutex<Project>>` (callers already hold a cloned
/// reference) and locks it itself. A new list is created with the requested kind
/// and an `array_base` equal to its position among the sequence's lists, then
/// pushed onto `sequence.track_lists` and given a back-reference to `seq` through
/// `TrackListBehavior::sequence`. The function does not push undo — callers
/// decide how the new list is recorded.
///
/// # Returns
///
/// The list token, or `None` when `seq` is not a live sequence node.
fn find_or_create_track_list(
	project: &Arc<Mutex<Project>>,
	seq: NodeId,
	kind: quercus_node::track::TrackType,
) -> Option<NodeId> {
	let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
	// Existing list of this kind?
	let existing = behavior_as::<SequenceBehavior>(&guard, seq)?.track_lists.iter().find_map(
		|&list| {
			let matches = behavior_as::<TrackListBehavior>(&guard, list)
				.is_some_and(|l| l.kind == kind);
			matches.then_some(list)
		},
	);
	if existing.is_some() {
		return existing;
	}
	let (core, mut behavior) = TrackListBehavior::create();
	if let Some(list) = behavior
		.as_any_mut()
		.and_then(|a| a.downcast_mut::<TrackListBehavior>())
	{
		list.kind = kind;
		list.array_base =
			behavior_as::<SequenceBehavior>(&guard, seq)?.track_lists.len() as i32;
	}
	let list_id = guard.graph.add_node(core, behavior);
	if let Some(s) = guard
		.graph
		.get_mut(seq)
		.and_then(|e| e.behavior.as_any_mut())
		.and_then(|a| a.downcast_mut::<SequenceBehavior>())
	{
		s.track_lists.push(list_id);
	}
	if let Some(l) = guard
		.graph
		.get_mut(list_id)
		.and_then(|e| e.behavior.as_any_mut())
		.and_then(|a| a.downcast_mut::<TrackListBehavior>())
	{
		l.sequence = Some(seq);
	}
	Some(list_id)
}

/// Adds a track of the requested media type to a sequence and returns its token.
///
/// `sequence` is the sequence token and `kind` is a `TrackType` code (`0` =
/// video, `1` = audio, `2` = subtitle; any other value is rejected). The matching
/// track list is found or created first (see `find_or_create_track_list`), then a
/// `TimelineAddTrackCommand` is pushed under "Add Track", which applies the
/// insertion and makes it undoable. The track is created by the command's
/// constructor, so its token exists once the push succeeds.
///
/// # Returns
///
/// `QUERCUS_NODE_ID_INVALID` for a null handle, a malformed `sequence` token, an
/// unknown `kind` code, a `sequence` that is not a sequence node, or a failed
/// undo push.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_add(
	project: *const ProjectHandle,
	sequence: u64,
	kind: i32,
) -> u64 {
	let Some(kind) = quercus_node::track::TrackType::from_c(kind) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let Some(seq) = node_id(sequence) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	if project.is_null() {
		return QUERCUS_NODE_ID_INVALID;
	}
	let project_ref = unsafe { &*project }.inner.clone();
	let Some(list) = find_or_create_track_list(&project_ref, seq, kind) else {
		return QUERCUS_NODE_ID_INVALID;
	};
	let track_cmd = quercus_timeline::undogeneral::TimelineAddTrackCommand::new(NodeRef::new(
		project_ref,
		list,
	));
	// The command creates the track in its constructor; `track()` reports
	// it. Push (which runs redo) makes it live and undoable.
	let track = track_cmd.track().id;
	if !push_undo(UndoCommand::new(track_cmd), "Add Track") {
		return QUERCUS_NODE_ID_INVALID;
	}
	track.identity()
}

// ---------------------------------------------------------------------------
// Block creation and track placement
// ---------------------------------------------------------------------------

macro_rules! block_create_fn {
	($( $name:ident => $create:expr, $doc:literal );* $(;)?) => {
		$(
			#[doc = $doc]
			/// Undo-free creation (placement on a track is the undoable
			/// step). Returns the block token, invalid on failure.
			#[unsafe(no_mangle)]
			pub unsafe extern "C" fn $name(project: *const ProjectHandle) -> u64 {
				with_project_mut(project, |guard| {
					let (core, behavior) = $create();
					guard.graph.add_node(core, behavior)
				})
				.map(|id| id.identity())
				.unwrap_or(QUERCUS_NODE_ID_INVALID)
			}
		)*
	};
}

block_create_fn! {
	quercus_block_create_clip => quercus_node::block::clip_create, "Creates a clip block (not on any track yet).";
	quercus_block_create_gap => quercus_node::block::gap_create, "Creates a gap block.";
	quercus_block_create_transition => quercus_node::block::transition_create, "Creates a transition block.";
	quercus_block_create_adjustment => quercus_node::block::adjustment_create, "Creates an adjustment block.";
}

/// Appends `block` to the end of `track` as one undoable "Insert Block" step.
///
/// Thin wrapper over `quercus_track_insert_block_after` with the invalid token as
/// `before`, so it shares that function's validation, ordering and undo
/// behaviour.
///
/// # Returns
///
/// `false` for a null handle, a malformed `track` or `block` token, or a failed
/// undo push.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_append_block(
	project: *const ProjectHandle,
	track: u64,
	block: u64,
) -> bool {
	quercus_track_insert_block_after(project, track, block, QUERCUS_NODE_ID_INVALID)
}

/// Places `block` on `track` immediately after `before`, as one undoable "Insert
/// Block" step.
///
/// `before` should be a block that already sits on `track`; passing the invalid
/// token (or any malformed token) appends at the end, which is what
/// `quercus_track_append_block` does. A failed undo push means nothing was
/// inserted.
///
/// # Returns
///
/// `false` for a null handle, a malformed `track` or `block` token, or a failed
/// undo push.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_insert_block_after(
	project: *const ProjectHandle,
	track: u64,
	block: u64,
	before: u64,
) -> bool {
	let (Some(track), Some(block)) = (node_id(track), node_id(block)) else {
		return false;
	};
	if project.is_null() {
		return false;
	}
	let project_ref = unsafe { &*project }.inner.clone();
	let before = node_id(before).map(|b| NodeRef::new(project_ref.clone(), b));
	push_undo(
		UndoCommand::new(quercus_timeline::undotrack::TrackInsertBlockAfterCommand::new(
			NodeRef::new(project_ref.clone(), track),
			NodeRef::new(project_ref, block),
			before,
		)),
		"Insert Block",
	)
}

/// Removes `block` from `track` and closes the gap by shifting the following
/// blocks earlier, as one undoable "Ripple Remove Block" step.
///
/// Both tokens must be well-formed; the timeline command performs the removal and
/// is a no-op when `block` is not actually on `track`. A failed undo push means no
/// removal happened.
///
/// # Returns
///
/// `false` for a null handle, a malformed `track` or `block` token, or a failed
/// undo push.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_ripple_remove_block(
	project: *const ProjectHandle,
	track: u64,
	block: u64,
) -> bool {
	let (Some(track), Some(block)) = (node_id(track), node_id(block)) else {
		return false;
	};
	if project.is_null() {
		return false;
	}
	let project_ref = unsafe { &*project }.inner.clone();
	push_undo(
		UndoCommand::new(quercus_timeline::undotrack::TrackRippleRemoveBlockCommand::new(
			NodeRef::new(project_ref.clone(), track),
			NodeRef::new(project_ref, block),
		)),
		"Ripple Remove Block",
	)
}

/// Replaces `old` with `new` on `track` as one undoable "Replace Block" step.
///
/// `track`, `old` and `new` must all be well-formed tokens; whether `old` is
/// actually present on the track is decided by the timeline command. Nothing is
/// applied when the undo push fails.
///
/// # Returns
///
/// `false` for a null handle, a malformed `track`/`old`/`new` token, or a failed
/// undo push.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_replace_block(
	project: *const ProjectHandle,
	track: u64,
	old: u64,
	new: u64,
) -> bool {
	let (Some(track), Some(old), Some(new)) = (node_id(track), node_id(old), node_id(new))
	else {
		return false;
	};
	if project.is_null() {
		return false;
	}
	let project_ref = unsafe { &*project }.inner.clone();
	push_undo(
		UndoCommand::new(quercus_timeline::undotrack::TrackReplaceBlockCommand::new(
			NodeRef::new(project_ref.clone(), track),
			NodeRef::new(project_ref.clone(), old),
			NodeRef::new(project_ref, new),
		)),
		"Replace Block",
	)
}

// ---------------------------------------------------------------------------
// Block trimming (undoable)
// ---------------------------------------------------------------------------

macro_rules! block_trim_fn {
	($( $name:ident => $cmd:ident, $label:literal );* $(;)?) => {
		$(
			#[doc = $label]
			/// Undoable. False on failure.
			#[unsafe(no_mangle)]
			pub unsafe extern "C" fn $name(
				project: *const ProjectHandle,
				block: u64,
				value: QuercusRational,
			) -> bool {
				use quercus_timeline::util::NodeRef;
				let Some(block) = node_id(block) else { return false };
				if project.is_null() {
					return false;
				}
				let project_ref = unsafe { &*project }.inner.clone();
				push_undo(
					UndoCommand::new(quercus_timeline::undogeneral::$cmd::new(
						NodeRef::new(project_ref, block),
						value.to_native(),
					)),
					$label,
				)
			}
		)*
	};
}

block_trim_fn! {
	quercus_block_set_length => BlockResizeCommand, "Set Block Length";
	quercus_block_set_length_and_media_in => BlockResizeWithMediaInCommand, "Set Block Length";
	quercus_block_set_media_in => BlockSetMediaInCommand, "Set Media In";
}

// ---------------------------------------------------------------------------
// Parameter writes (undoable)
// ---------------------------------------------------------------------------

/// Sets one input's static value as a single undoable "Set Parameter" step.
///
/// Only POD values are bridged: `value` is a `QuercusNodeValue` tagged with its
/// kind and is converted to the input's declared type. `input_id` names the
/// declared input and `element` is the array element index, or `-1` for a scalar
/// input. The previous value of exactly that (input, element) pair is captured,
/// so undo restores it without resetting the rest of the node, and the undo
/// command is only constructed after the node, input and type checks pass.
///
/// # Returns
///
/// `false` for a null handle, a malformed `id`, a null/non-UTF-8 `input_id`, an
/// unknown node or input, a value incompatible with the declared type, a string
/// input, or a failed undo push.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `input_id` must
/// be null or point to `input_id_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_node_set_standard_value(
	project: *const ProjectHandle,
	id: u64,
	input_id: *const u8,
	input_id_len: usize,
	element: i32,
	value: quercus_node::value::QuercusNodeValue,
) -> bool {
	let Some(id) = node_id(id) else { return false };
	let Some(input) = (unsafe { str_arg(input_id, input_id_len) }) else {
		return false;
	};
	if project.is_null() {
		return false;
	}
	set_standard_value_impl(unsafe { &*project }.project(), id, input, element, value).is_ok()
}

/// Sets one declared input's static POD value as a single undoable parameter edit.
///
/// The input's declared type drives conversion from the ABI value. Unknown
/// nodes/inputs and incompatible values return an error before an undo command
/// is constructed. The previous value is captured per input/element so undo
/// restores exactly that value rather than resetting the whole node.
pub(crate) fn set_standard_value_impl(
	project_ref: &Arc<Mutex<Project>>,
	id: NodeId,
	input: &str,
	element: i32,
	value: quercus_node::value::QuercusNodeValue,
) -> Result<(), String> {
	let (new_value, old_value) = {
		let guard = project_ref.lock().unwrap_or_else(|e| e.into_inner());
		let Some(entry) = guard.graph.get(id) else {
			return Err("set parameter: node not found".into());
		};
		let Some(declared) = entry.core.input_data_type(&input) else {
			return Err(format!("set parameter: no input \"{input}\""));
		};
		let Ok(new_value) = value.to_node_value(declared) else {
			return Err(format!("set parameter: incompatible value for \"{input}\""));
		};
		let old_value = entry.core.standard_value(&input, element);
		(new_value, old_value)
	};

	let project_redo = project_ref.clone();
	let project_undo = project_ref.clone();
	let input_redo = input.to_string();
	let input_undo = input.to_string();
	quercus_undo::global::push(
		quercus_undo::undocommand::UndoCommand::from_closures(
			move || {
				let mut g = project_redo.lock().unwrap_or_else(|e| e.into_inner());
				if let Some(entry) = g.graph.get_mut(id) {
					entry.core.set_standard_value(&input_redo, element, new_value.clone());
				}
			},
			move || {
				let mut g = project_undo.lock().unwrap_or_else(|e| e.into_inner());
				if let Some(entry) = g.graph.get_mut(id) {
					entry.core.set_standard_value(&input_undo, element, old_value.clone());
				}
			},
		),
		"Set Parameter",
	)
	.map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Graph edges (document structure: persisted in the project file)
// ---------------------------------------------------------------------------

/// Connects `from`'s output to `to`'s `input_id` at `element` in the project
/// graph.
///
/// `input_id` is the declared input name on `to`, and `element` is the array
/// element index or `-1` for a scalar input. The edge is written directly (not an
/// undoable step) and is part of the document that `quercus_project_save`
/// persists. The graph rejects a non-connectable input, a scalar input addressed
/// with an element other than `-1`, an input that already has a connection, a
/// self-connection, and any edge that would create a cycle.
///
/// # Returns
///
/// `false` for a null handle, malformed `from`/`to` tokens, a null/non-UTF-8
/// `input_id`, or any of the graph rejections above.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `input_id` must
/// be null or point to `input_id_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_node_connect(
	project: *const ProjectHandle,
	from: u64,
	to: u64,
	input_id: *const u8,
	input_id_len: usize,
	element: i32,
) -> bool {
	let (Some(from), Some(to)) = (node_id(from), node_id(to)) else {
		return false;
	};
	let Some(input) = (unsafe { str_arg(input_id, input_id_len) }) else {
		return false;
	};
	with_project_mut(project, |guard| guard.graph.connect(from, to, input, element).is_ok())
		.unwrap_or(false)
}

/// Disconnects whatever feeds `to`'s `input_id` at `element`.
///
/// `input_id` is the declared input name on `to` and `element` is the array
/// element index, or `-1` for a scalar input. This is a direct graph write: it is
/// not an undoable step and does not set the project's modified flag.
/// Disconnecting an input that has no edge is a no-op and still succeeds.
///
/// # Returns
///
/// `false` only for a null handle, a malformed `to` token, or a null/non-UTF-8
/// `input_id`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `input_id` must
/// be null or point to `input_id_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_node_disconnect(
	project: *const ProjectHandle,
	to: u64,
	input_id: *const u8,
	input_id_len: usize,
	element: i32,
) -> bool {
	let Some(to) = node_id(to) else { return false };
	let Some(input) = (unsafe { str_arg(input_id, input_id_len) }) else {
		return false;
	};
	with_project_mut(project, |guard| {
		guard.graph.disconnect_input(to, input, element);
		true
	})
	.unwrap_or(false)
}

#[cfg(test)]
mod edit_tests {
	use super::*;


	fn temp_clip(name: &str) -> String {
		let dir = std::env::temp_dir().join(format!(
			"quercusengine_nodeedit_{}_{}",
			name,
			std::process::id()
		));
		let _ = std::fs::create_dir_all(&dir);
		let path = dir.join("clip.mp4");
		quercus_codec::testmedia::write_test_clip(&path, 64, 64, 10, 10).expect("generate test media");
		path.to_string_lossy().into_owned()
	}

	fn arg(s: &str) -> (*const u8, usize) {
		(s.as_ptr(), s.len())
	}

	/// The money test: import footage → create sequence → create clip →
	/// place on track → trim → undo the whole chain back to the empty
	/// project.
	#[test]
	fn edit_chain_and_undo_rollback() {
		let _guard = super::UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		unsafe {
			let project = quercus_project_new();
			let undo_base = quercus_undo_count();

			// Import footage (undoable).
			let clip_path = temp_clip("chain");
			let (f, fl) = arg(&clip_path);
			let footage = quercus_footage_import(project, f, fl);
			assert!(quercus_node_id_is_valid(footage), "import the test clip");
			let root = quercus_project_root(project);
			assert_eq!(quercus_folder_child_count(project, root), 1);
			assert_eq!(quercus_folder_child_at(project, root, 0), footage);
			assert!(quercus_footage_video_stream_count(project, footage) >= 1);

			// Create a sequence (creation itself is not undoable, like the
			// app's flow).
			let (n, nl) = arg("Seq 1");
			let seq = quercus_sequence_create(project, n, nl);
			assert!(quercus_node_id_is_valid(seq));
			assert_eq!(quercus_sequence_track_list_count(project, seq), 2);

			// Create a clip and place it on the first video track.
			let clip = quercus_block_create_clip(project);
			assert!(quercus_node_id_is_valid(clip));
			assert_eq!(quercus_node_kind(project, clip), 5);

			let tracklist = quercus_sequence_track_list_at(project, seq, 0);
			let track = quercus_tracklist_track_at(project, tracklist, 0);
			assert!(quercus_track_append_block(project, track, clip));
			assert_eq!(quercus_track_block_count(project, track), 1);
			assert_eq!(quercus_track_block_at(project, track, 0), clip);

			// Trim it.
			assert!(quercus_block_set_length(
				project,
				clip,
				QuercusRational { num: 5, den: 1 }
			));
			assert_eq!(
				quercus_block_length(project, clip),
				QuercusRational { num: 5, den: 1 }
			);

			// Undo: trim, placement, import — in reverse order.
			assert!(quercus_undo_undo(), "undo the trim");
			assert_ne!(
				quercus_block_length(project, clip),
				QuercusRational { num: 5, den: 1 },
				"the trim was rolled back"
			);
			assert!(quercus_undo_undo(), "undo the placement");
			assert_eq!(quercus_track_block_count(project, track), 0);
			assert!(quercus_undo_undo(), "undo the import");
			// The sequence stays: its mount is part of its creation, not
			// an undoable edit (same as the app).
			assert_eq!(quercus_folder_child_count(project, root), 1);
			assert_eq!(quercus_folder_child_at(project, root, 0), seq);

			// Redo everything.
			assert!(quercus_undo_redo());
			assert!(quercus_undo_redo());
			assert!(quercus_undo_redo());
			assert_eq!(quercus_folder_child_count(project, root), 2);
			assert_eq!(quercus_track_block_count(project, track), 1);
			assert_eq!(
				quercus_block_length(project, clip),
				QuercusRational { num: 5, den: 1 }
			);
			assert_eq!(quercus_undo_count(), undo_base + 3);

			quercus_project_release(project);
		}
	}

	#[test]
	fn undo_groups_batch_edits() {
		let _guard = super::UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		unsafe {
			let project = quercus_project_new();
			let base = quercus_undo_count();

			let (n, nl) = arg("Import Two Clips");
			assert!(quercus_undo_group_begin(n, nl));
			let clip_path = temp_clip("group");
			let (f, fl) = arg(&clip_path);
			let f1 = quercus_footage_import(project, f, fl);
			assert!(quercus_node_id_is_valid(f1));
			assert!(quercus_undo_group_end());
			assert_eq!(quercus_undo_count(), base + 1, "the group is one undo step");

			assert!(quercus_undo_undo());
			assert!(quercus_undo_redo());

			// group_abort rolls back and records nothing.
			let (n, nl) = arg("aborted");
			assert!(quercus_undo_group_begin(n, nl));
			let f2 = quercus_footage_import(project, f, fl);
			assert!(quercus_node_id_is_valid(f2));
			assert!(quercus_undo_group_abort());
			assert_eq!(quercus_undo_count(), base + 1, "abort records nothing");
			let root = quercus_project_root(project);
			assert_eq!(quercus_folder_child_count(project, root), 1, "aborted import rolled back");

			quercus_project_release(project);
		}
	}

	#[test]
	fn param_write_is_undoable() {
		let _guard = super::UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		unsafe {
			let project = quercus_project_new();
			let clip = quercus_block_create_clip(project);
			let (i, il) = arg("media_in_in");

			let mut value = quercus_node::value::QuercusNodeValue::none();
			value.kind = quercus_node::value::quercus::RATIONAL;
			value.num = 7;
			value.den = 1;
			assert!(quercus_node_set_standard_value(project, clip, i, il, -1, value));

			let mut out = quercus_node::value::QuercusNodeValue::none();
			assert!(quercus_node_standard_value(project, clip, i, il, -1, &mut out));
			assert_eq!((out.num, out.den), (7, 1));

			assert!(quercus_undo_undo());
			assert!(quercus_node_standard_value(project, clip, i, il, -1, &mut out));
			assert_eq!((out.num, out.den), (0, 1), "undo restored the old value");
			assert!(quercus_undo_redo());
			assert!(quercus_node_standard_value(project, clip, i, il, -1, &mut out));
			assert_eq!((out.num, out.den), (7, 1));

			// Unknown input id fails cleanly.
			let (i, il) = arg("no_such_input");
			assert!(!quercus_node_set_standard_value(project, clip, i, il, -1, value));

			quercus_project_release(project);
		}
	}

	#[test]
	fn edit_null_safety() {
		let _guard = super::UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		unsafe {
			let project = quercus_project_new();
			assert_eq!(quercus_footage_import(project, std::ptr::null(), 0), QUERCUS_NODE_ID_INVALID);
			assert_eq!(
				quercus_footage_import(project, "/nonexistent/x.mp4".as_ptr(), 17),
				QUERCUS_NODE_ID_INVALID
			);
			assert_eq!(quercus_folder_create(std::ptr::null(), 1, "f".as_ptr(), 1), QUERCUS_NODE_ID_INVALID);
			assert_eq!(quercus_sequence_create(std::ptr::null(), "s".as_ptr(), 1), QUERCUS_NODE_ID_INVALID);
			assert!(!quercus_node_remove(project, QUERCUS_NODE_ID_INVALID));
			// A well-formed token naming no live node is a no-op command:
			// either outcome is acceptable, it just must not crash.
			let _ = quercus_node_remove(project, 1);
			assert!(!quercus_track_append_block(project, QUERCUS_NODE_ID_INVALID, 2));
			assert!(!quercus_track_ripple_remove_block(project, QUERCUS_NODE_ID_INVALID, 2));
			assert!(!quercus_block_set_length(project, QUERCUS_NODE_ID_INVALID, QuercusRational { num: 1, den: 1 }));
			assert!(!quercus_node_connect(project, 1, QUERCUS_NODE_ID_INVALID, "x".as_ptr(), 1, -1));
			// On a non-empty stack this undoes a real entry; redo it
			// immediately — a redoable tail would be dropped by the next
			// push, shifting the global count out from under the other
			// tests (UNDO_SERIAL serializes the stack, not the tail).
			if quercus_undo_undo() {
				assert!(quercus_undo_redo());
			}
			quercus_project_release(project);
		}
	}
}

// ---------------------------------------------------------------------------
// Batch 3: project persistence and settings
// ---------------------------------------------------------------------------

/// Serializes the project to its XML byte representation — the same
/// document the path-based [`quercus_project_save`] writes.
///
/// Uses the crate's snprintf-style buffer convention: returns the full
/// document length in bytes excluding the trailing NUL, and writes
/// `min(len, cap - 1)` bytes plus a NUL. A null `buf` or `cap == 0` is a
/// size query; a return value `>= cap` means the document did not fit (a
/// `cap`-byte buffer holds at most `cap - 1` document bytes plus the
/// NUL). Negative returns are `QUERCUSSTORAGE_E_*` codes: `E_INVALID`
/// for a null project and `E_FORMAT` when serialization fails. The
/// `i32` return bounds a document to `i32::MAX` bytes.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller, and `buf` must
/// be null or writable for `cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_save_to_bytes(
	project: *const ProjectHandle,
	buf: *mut u8,
	cap: usize,
) -> i32 {
	let Some(project) = (unsafe { project.as_ref() }) else {
		return QUERCUSSTORAGE_E_INVALID;
	};
	let guard = project
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let xml = match quercus_node::serializer::save(&guard) {
		Ok(xml) => xml,
		Err(_) => return QUERCUSSTORAGE_E_FORMAT,
	};
	copy_str_out(&xml, buf, cap)
}

/// Loads a project from its XML byte representation. Returns an owned
/// handle (reference count 1) or null. `Project::filename` stays empty:
/// bytes carry no path, so callers that want save-as semantics set it
/// with [`quercus_project_set_filename`].
///
/// `*version_info` receives one of `QUERCUSSTORAGE_OK`,
/// `QUERCUSSTORAGE_TOO_OLD`, `QUERCUSSTORAGE_TOO_NEW` or
/// `QUERCUSSTORAGE_UNKNOWN_VERSION`. TOO_NEW/UNKNOWN return null with
/// `*error == 0` (the document is intact but this build cannot load it).
/// A genuine failure returns null with a negative `QUERCUSSTORAGE_E_*`
/// code in `*error`: `E_INVALID` for null pointers and `E_FORMAT` for
/// non-UTF-8, unparseable XML or a serializer failure.
///
/// # Safety
/// `bytes` must point to `len` readable bytes (or be null). `version_info`
/// and `error` must both be writable `i32` slots: the function fails closed
/// (returns null) when either is null, reporting `E_INVALID` through
/// `error` when that slot is available.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_load_from_bytes(
	bytes: *const u8,
	len: usize,
	version_info: *mut i32,
	error: *mut i32,
) -> *mut ProjectHandle {
	let Some(error) = (unsafe { error.as_mut() }) else {
		return std::ptr::null_mut();
	};
	let Some(version_info) = (unsafe { version_info.as_mut() }) else {
		*error = QUERCUSSTORAGE_E_INVALID;
		return std::ptr::null_mut();
	};
	*version_info = QUERCUSSTORAGE_OK;
	*error = 0;
	if bytes.is_null() {
		*error = QUERCUSSTORAGE_E_INVALID;
		return std::ptr::null_mut();
	}
	let Some(xml) = (unsafe { str_arg(bytes, len) }) else {
		*error = QUERCUSSTORAGE_E_FORMAT;
		return std::ptr::null_mut();
	};
	let Some(probe) = quercus_node::serializer::probe_version(xml) else {
		*error = QUERCUSSTORAGE_E_FORMAT;
		return std::ptr::null_mut();
	};
	match probe {
		quercus_node::serializer::VersionProbe::TooNew => {
			*version_info = QUERCUSSTORAGE_TOO_NEW;
			return std::ptr::null_mut();
		}
		quercus_node::serializer::VersionProbe::Unknown => {
			*version_info = QUERCUSSTORAGE_UNKNOWN_VERSION;
			return std::ptr::null_mut();
		}
		quercus_node::serializer::VersionProbe::Current
		| quercus_node::serializer::VersionProbe::Old => {}
	}
	let project = match quercus_node::serializer::load(xml) {
		Ok(project) => project,
		Err(_) => {
			*error = QUERCUSSTORAGE_E_FORMAT;
			return std::ptr::null_mut();
		}
	};
	if probe == quercus_node::serializer::VersionProbe::Old {
		*version_info = QUERCUSSTORAGE_TOO_OLD;
	}
	crate::handle::into_ffi(ProjectHandle { inner: project }) as *mut ProjectHandle
}

/// Saves the project as XML to `path` (null/empty = the project's own
/// filename; the path becomes the project's filename, save-as
/// semantics). False when no path is available or on IO/serialize
/// error. The bytes come from [`quercus_project_save_to_bytes`], so the
/// in-memory and on-disk encodings are always the same.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `path` must be
/// null or point to `path_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_save(
	project: *const ProjectHandle,
	path: *const u8,
	path_len: usize,
) -> bool {
	if project.is_null() {
		return false;
	}
	let path = if path.is_null() || path_len == 0 {
		None
	} else {
		match unsafe { str_arg(path, path_len) } {
			Some(p) => Some(p.to_string()),
			None => return false,
		}
	};
	// Resolve the target path under the lock (save-as semantics), then
	// serialize through the byte primitive (which takes the lock itself).
	let target = {
		let guard = unsafe { &*project }
			.inner
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		match path {
			Some(p) => p,
			None => {
				if guard.filename.is_empty() {
					return false;
				}
				guard.filename.clone()
			}
		}
	};
	let len = unsafe { quercus_project_save_to_bytes(project, std::ptr::null_mut(), 0) };
	if len < 0 {
		return false;
	}
	let mut bytes = vec![0u8; len as usize + 1];
	let written =
		unsafe { quercus_project_save_to_bytes(project, bytes.as_mut_ptr(), bytes.len()) };
	if written != len {
		// The project changed between the size query and the write.
		return false;
	}
	if std::fs::write(&target, &bytes[..len as usize]).is_err() {
		return false;
	}
	let mut guard = unsafe { &*project }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	guard.filename = target;
	guard.modified = false;
	true
}

/// Loads a project from an XML file. Null on failure. The project is
/// NOT made active automatically. Delegates to
/// [`quercus_project_load_from_bytes`] and then records the path.
///
/// # Safety
///
/// `path` must be null or point to `path_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_load(
	path: *const u8,
	path_len: usize,
) -> *mut ProjectHandle {
	let Some(path) = (unsafe { str_arg(path, path_len) }) else {
		return std::ptr::null_mut();
	};
	let bytes = match std::fs::read(path) {
		Ok(bytes) => bytes,
		Err(_) => return std::ptr::null_mut(),
	};
	let mut version_info = QUERCUSSTORAGE_OK;
	let mut error = 0;
	let project = unsafe {
		quercus_project_load_from_bytes(
			bytes.as_ptr(),
			bytes.len(),
			&mut version_info,
			&mut error,
		)
	};
	if project.is_null() {
		return std::ptr::null_mut();
	}
	unsafe { &*project }
		.inner
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.filename = path.to_string();
	project
}

/// Sets the project's file path without marking the project modified.
///
/// The path is what `quercus_project_save` falls back to when it is called with
/// a null or empty path, and it is the directory source for the cache-location
/// mode "alongside the project". Passing an empty path clears the filename,
/// making the project unsaved again.
///
/// # Returns
///
/// `false` for a null handle or a null/non-UTF-8 `path`; `true` otherwise.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `path` must be
/// null or point to `path_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_set_filename(
	project: *const ProjectHandle,
	path: *const u8,
	path_len: usize,
) -> bool {
	let Some(path) = (unsafe { str_arg(path, path_len) }) else {
		return false;
	};
	with_project_mut(project, |guard| {
		guard.filename = path.to_string();
	})
	.is_some()
}

/// Sets the working color space in the project settings and applies it to the
/// process-wide color pipeline.
///
/// `setting` is matched case-insensitively: `"srgb"`, `"srgb_legacy"` and
/// `"legacy"` select the sRGB-legacy space; every other string selects ACEScg.
/// The canonical value is stored under the `workingcolorspace` project setting
/// (so it round-trips through save/load), and the global pipeline is updated
/// immediately: the change affects every subsequent render in this process, not
/// only this project. The project's dirty flag is left unchanged.
///
/// # Returns
///
/// `false` for a null handle or a null/non-UTF-8 `setting`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `setting` must
/// be null or point to `setting_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_set_working_color_space(
	project: *const ProjectHandle,
	setting: *const u8,
	setting_len: usize,
) -> bool {
	let Some(setting) = (unsafe { str_arg(setting, setting_len) }) else {
		return false;
	};
	let space = quercus_core::colormath::WorkingColorSpace::from_setting(setting);
	if with_project_mut(project, |guard| guard.set_working_color_space(space)).is_none() {
		return false;
	}
	let spec = quercus_core::color::pipeline_output_spec();
	quercus_core::color::set_pipeline_color_settings(space, spec);
	true
}

/// Sets the delivery gamut and transfer function in the project settings and
/// applies them to the process-wide color pipeline.
///
/// `gamut` accepts `"srgb"`, `"displayp3"`/`"p3"`, and
/// `"bt2020"`/`"rec2020"`/`"2020"`; `transfer` accepts `"srgb"`,
/// `"gamma22"`/`"2.2"`, `"pq"`/`"st2084"`, and `"hlg"`. Both matches are
/// case-insensitive and an unrecognized string falls back to sRGB. The
/// canonical spellings are stored under the `outputgamut` and `outputtransfer`
/// settings, and the global pipeline is updated together with the current
/// working space, affecting every subsequent render in the process.
///
/// # Returns
///
/// `false` for a null handle or a null/non-UTF-8 `gamut` or `transfer`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller;
/// `gamut`/`transfer` must be null or point to their stated readable lengths of
/// valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_set_output_color_spec(
	project: *const ProjectHandle,
	gamut: *const u8,
	gamut_len: usize,
	transfer: *const u8,
	transfer_len: usize,
) -> bool {
	let (Some(gamut), Some(transfer)) = (
		unsafe { str_arg(gamut, gamut_len) },
		unsafe { str_arg(transfer, transfer_len) },
	) else {
		return false;
	};
	let spec = quercus_core::colormath::OutputColorSpec::from_settings(gamut, transfer);
	if with_project_mut(project, |guard| guard.set_output_color_spec(spec)).is_none() {
		return false;
	}
	let working = quercus_core::color::pipeline_working_space();
	quercus_core::color::set_pipeline_color_settings(working, spec);
	true
}

/// Returns an independent deep copy of the project, used to isolate background
/// renders from edits made while they run.
///
/// The copy is taken under the project lock and owns its own graph and node
/// behaviours, so mutating either project does not affect the other. The
/// returned handle has reference count 1 and must be released with
/// `quercus_project_release`; it is not installed as the active project (see
/// `quercus_project_set_active`).
///
/// # Returns
///
/// Null for a null handle, or when the copy fails (for example because the
/// project lock is poisoned); otherwise an owned `ProjectHandle*`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_deep_copy(
	project: *const ProjectHandle,
) -> *mut ProjectHandle {
	if project.is_null() {
		return std::ptr::null_mut();
	}
	let copy = {
		let guard = unsafe { &*project }
			.inner
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		guard.deep_copy()
	};
	match copy {
		Ok(inner) => crate::handle::into_ffi(ProjectHandle { inner }) as *mut ProjectHandle,
		Err(_) => std::ptr::null_mut(),
	}
}

/// The project's library UUID, snprintf-style.
///
/// The UUID is generated when the project is created, is persisted in the
/// project XML, and is used as the base name for cache files. Follows the
/// crate's string convention: the full byte length excluding the NUL is
/// returned, `min(len, buf_len - 1)` bytes plus a NUL are written, a null `buf`
/// (or `buf_len == 0`) is a size query, and `ret >= buf_len` means the output
/// was truncated.
///
/// # Returns
///
/// `-1` for a null handle.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `buf` must be
/// null or writable for `buf_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_uuid(
	project: *const ProjectHandle,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	write_str(
		with_project(project, |p| p.uuid.clone()),
		|value, buf, len| copy_str_out(&value, buf, len),
		buf,
		buf_len,
	)
}

/// Reads an arbitrary project setting value, snprintf-style.
///
/// `key` is a settings-map key such as `"workingcolorspace"`, `"outputgamut"`,
/// `"outputtransfer"`, `"cachesetting"` or `"customcachepath"`. The same string
/// convention as `quercus_project_uuid` applies: the full byte length excluding
/// the NUL is returned, a null `buf` (or `buf_len == 0`) is a size query, and
/// `ret >= buf_len` means the caller's buffer was too small.
///
/// # Returns
///
/// `-1` for a null handle, a null/non-UTF-8 key, or a key that is not set.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `key` must be
/// null or point to `key_len` readable bytes of valid UTF-8, and `buf` must be
/// null or writable for `buf_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_setting(
	project: *const ProjectHandle,
	key: *const u8,
	key_len: usize,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let Some(key) = (unsafe { str_arg(key, key_len) }) else {
		return -1;
	};
	write_str(
		with_project(project, |p| p.settings.get(key).cloned()).flatten(),
		|value, buf, len| copy_str_out(value, buf, len),
		buf,
		buf_len,
	)
}

/// Writes a project setting and marks the project modified.
///
/// `key`/`value` are copied verbatim into the settings map; an empty key is
/// rejected but an empty value is allowed (it stores an empty string). This is
/// the generic writer behind the cache and color-management metadata, so hosts
/// can persist their own strings alongside them.
///
/// # Returns
///
/// `false` for a null handle, an empty key, or a null/non-UTF-8 key or value.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller;
/// `key`/`value` must be null or point to their stated readable lengths of
/// valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_set_setting(
	project: *const ProjectHandle,
	key: *const u8,
	key_len: usize,
	value: *const u8,
	value_len: usize,
) -> bool {
	let (Some(key), Some(value)) = (unsafe { str_arg(key, key_len) }, unsafe {
		str_arg(value, value_len)
	}) else {
		return false;
	};
	if key.is_empty() {
		return false;
	}
	with_project_mut(project, |guard| {
		guard.settings.insert(key.to_string(), value.to_string());
		guard.set_modified(true);
	})
	.is_some()
}

/// Reads the project's cache policy and custom cache path.
///
/// The policy is written to `*setting` when that pointer is non-null: `0` =
/// default location, `1` = alongside the project file, `2` = custom path. The
/// stored custom path is copied into `path` snprintf-style (full byte length
/// excluding the NUL returned; a null `path` or `path_len == 0` is a size
/// query). Pair with `quercus_project_set_cache_location`.
///
/// # Returns
///
/// The custom path's byte length, or `-1` for a null handle.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `setting` must
/// be null or writable for one `i32`; `path` must be null or writable for
/// `path_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_cache_location(
	project: *const ProjectHandle,
	setting: *mut i32,
	path: *mut u8,
	path_len: usize,
) -> i32 {
	let Some((mode, custom)) = with_project(project, |p| {
		(p.cache_location_setting, p.custom_cache_path.clone())
	}) else {
		return -1;
	};
	if !setting.is_null() {
		unsafe { *setting = mode };
	}
	copy_str_out(&custom, path, path_len)
}

/// Sets the cache policy and custom cache path and marks the project modified.
///
/// `setting` is clamped to `0..=2` (`0` = default, `1` = alongside the project,
/// `2` = custom). `path` is stored only when the clamped policy is `2` (trimmed
/// of surrounding whitespace); for the other policies the custom path is
/// cleared, so switching back to the default cannot leave a stale path behind.
///
/// # Returns
///
/// `false` for a null handle or a null/non-UTF-8 `path`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller; `path` must be
/// null or point to `path_len` readable bytes of valid UTF-8.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_project_set_cache_location(
	project: *const ProjectHandle,
	setting: i32,
	path: *const u8,
	path_len: usize,
) -> bool {
	let Some(path) = (unsafe { str_arg(path, path_len) }) else {
		return false;
	};
	with_project_mut(project, |guard| {
		guard.cache_location_setting = setting.clamp(0, 2);
		guard.custom_cache_path = if guard.cache_location_setting == 2 {
			path.trim().to_string()
		} else {
			String::new()
		};
		guard.set_modified(true);
	})
	.is_some()
}

/// The sequence's interlaced flag as an `int`: `1` interlaced, `0` progressive,
/// `-1` unavailable.
///
/// The value is read from the sequence's first video stream. `-1` is returned
/// for a null handle, an unknown/stale token, a non-sequence node, or a sequence
/// with no video stream parameters — the same cases where the flag cannot be
/// known. Pair with `quercus_sequence_set_interlaced`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_sequence_interlaced(project: *const ProjectHandle, id: u64) -> i32 {
	node_id(id)
		.and_then(|id| {
			with_project(project, |p| {
				behavior_as::<SequenceBehavior>(p, id)
					.and_then(|seq| seq.video_params.first().map(|params| params.interlaced))
			})
			.flatten()
		})
		.map_or(-1, |value| value as i32)
}

/// The track's height in internal height units (a `double`, not pixels).
///
/// This is the raw value stored on the track and used by timeline snapshots; one
/// unit is the default font height (13 px), so the default height of 3.0 is
/// 39 px. `quercus_track_height_pixels` returns the display height in pixels
/// with the 24 px floor applied.
///
/// # Returns
///
/// `0.0` for a null handle, an unknown/stale token, or a non-track node.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_height(project: *const ProjectHandle, id: u64) -> f64 {
	node_id(id)
		.and_then(|id| with_project(project, |p| behavior_as::<TrackBehavior>(p, id).map(|t| t.height)).flatten())
		.unwrap_or(0.0)
}

/// Whether the track is muted.
///
/// Muted tracks are skipped by the preview and render consumers (video montage,
/// audio mix and render) while their blocks stay in the model, so unmuting
/// restores the output without any rebuild.
///
/// # Returns
///
/// `false` for a null handle, an unknown/stale token, or a non-track node —
/// which is also the value for an unmuted track. Pair with
/// `quercus_track_locked`.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_muted(project: *const ProjectHandle, id: u64) -> bool {
	node_id(id)
		.and_then(|id| with_project(project, |p| behavior_as::<TrackBehavior>(p, id).map(|t| t.muted)).flatten())
		.unwrap_or(false)
}

/// Whether the track is locked.
///
/// The flag is the editing guard read by the timeline operations
/// (`quercus-timeline`); the editing entry points in this C ABI do not check it,
/// so a host that wants locking enforced must test the flag itself before
/// calling `quercus_track_append_block` and friends.
///
/// # Returns
///
/// `false` for a null handle, an unknown/stale token, or a non-track node.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_track_locked(project: *const ProjectHandle, id: u64) -> bool {
	node_id(id)
		.and_then(|id| with_project(project, |p| behavior_as::<TrackBehavior>(p, id).map(|t| t.locked)).flatten())
		.unwrap_or(false)
}

/// Whether a block is enabled (its standard "enabled" flag).
///
/// Reads `BlockCore::enabled`, which all four block kinds (clip, gap,
/// transition, adjustment) carry. A disabled block stays on its track and keeps
/// its range, so editing operations still see it; consumers that honour the flag
/// skip its contribution.
///
/// # Returns
///
/// `false` for a null handle, an unknown/stale token, a non-block node, or a
/// block whose flag is cleared.
///
/// # Safety
///
/// `project` must be null or a live handle owned by the caller for the duration
/// of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_block_enabled(project: *const ProjectHandle, id: u64) -> bool {
	node_id(id)
		.and_then(|id| {
			with_project(project, |p| {
				p.graph.get(id).and_then(|entry| {
					entry.behavior.as_any().and_then(|behavior| {
						if let Some(block) = behavior.downcast_ref::<ClipBlockBehavior>() {
							Some(block.core.enabled)
						} else if let Some(block) = behavior.downcast_ref::<GapBlockBehavior>() {
							Some(block.core.enabled)
						} else if let Some(block) = behavior.downcast_ref::<TransitionBlockBehavior>() {
							Some(block.core.enabled)
						} else if let Some(block) = behavior.downcast_ref::<AdjustmentBlockBehavior>() {
							Some(block.core.enabled)
						} else {
							None
						}
					})
				})
			})
			.flatten()
		})
		.unwrap_or(false)
}

#[cfg(test)]
mod persist_tests {
	use super::*;

	fn arg(s: &str) -> (*const u8, usize) {
		(s.as_ptr(), s.len())
	}

	#[test]
	fn save_load_roundtrip() {
		let _guard = super::UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		unsafe {
			let project = quercus_project_new();
			let (n, nl) = arg("Persisted Folder");
			let folder = quercus_folder_create(project, quercus_project_root(project), n, nl);
			assert!(quercus_node_id_is_valid(folder));

			let path = std::env::temp_dir()
				.join(format!("quercusengine_persist_{}.ove", std::process::id()))
				.to_string_lossy()
				.into_owned();
			let (p, pl) = arg(&path);
			assert!(quercus_project_save(project, p, pl));
			assert!(!quercus_project_modified(project), "saved projects are clean");

			let loaded = quercus_project_load(p, pl);
			assert!(!loaded.is_null(), "reload the saved project");
			let root = quercus_project_root(loaded);
			assert_eq!(quercus_folder_child_count(loaded, root), 1);
			assert_eq!(quercus_folder_child_at(loaded, root, 0), folder,
				"NodeIds survive the round trip");

			quercus_project_release(loaded);
			quercus_project_release(project);
			let _ = std::fs::remove_file(&path);

			// Negative paths.
			assert!(!quercus_project_save(std::ptr::null(), p, pl));
			assert!(quercus_project_load("nope".as_ptr(), 4).is_null());
		}
	}

	#[test]
	fn bytes_roundtrip_and_size_query() {
		let _guard = super::UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		unsafe {
			let project = quercus_project_new();
			let (n, nl) = arg("Bytes Folder");
			let folder = quercus_folder_create(project, quercus_project_root(project), n, nl);
			assert!(quercus_node_id_is_valid(folder));
			let node_count = quercus_project_node_count(project);

			let mut uuid_buf = [0u8; 64];
			let uuid_len = quercus_project_uuid(project, uuid_buf.as_mut_ptr(), uuid_buf.len());
			let uuid = std::str::from_utf8(&uuid_buf[..uuid_len as usize])
				.unwrap()
				.to_string();
			let mut name_buf = [0u8; 64];
			let name_len = quercus_project_name(project, name_buf.as_mut_ptr(), name_buf.len());
			let name = std::str::from_utf8(&name_buf[..name_len as usize])
				.unwrap()
				.to_string();

			// Size query: a null buffer / cap 0 returns the document length.
			let len = quercus_project_save_to_bytes(project, std::ptr::null_mut(), 0);
			assert!(len > 0);

			// Exact fit: `len` bytes + the NUL.
			let mut bytes = vec![0u8; len as usize + 1];
			assert_eq!(
				quercus_project_save_to_bytes(project, bytes.as_mut_ptr(), bytes.len()),
				len
			);
			assert_eq!(bytes.pop(), Some(0), "the output is NUL-terminated");

			// Too-small cap: the full length is still reported and the
			// output is truncated + NUL-terminated.
			let mut small = [0xAAu8; 8];
			assert_eq!(
				quercus_project_save_to_bytes(project, small.as_mut_ptr(), small.len()),
				len
			);
			assert_eq!(&small[..7], &bytes[..7]);
			assert_eq!(small[7], 0, "truncated output is NUL-terminated");

			// Null project: invalid argument.
			assert_eq!(
				quercus_project_save_to_bytes(std::ptr::null(), std::ptr::null_mut(), 0),
				QUERCUSSTORAGE_E_INVALID
			);

			// The path-based save writes exactly the in-memory bytes.
			let path = std::env::temp_dir()
				.join(format!("quercusengine_bytes_{}.ove", std::process::id()))
				.to_string_lossy()
				.into_owned();
			let (p, pl) = arg(&path);
			assert!(quercus_project_save(project, p, pl));
			assert_eq!(std::fs::read(&path).unwrap(), bytes);

			let mut version_info = -1;
			let mut error = -1;
			let loaded = quercus_project_load_from_bytes(
				bytes.as_ptr(),
				bytes.len(),
				&mut version_info,
				&mut error,
			);
			assert!(!loaded.is_null(), "reload the byte-saved project");
			assert_eq!(version_info, QUERCUSSTORAGE_OK);
			assert_eq!(error, 0);
			assert_eq!(quercus_project_node_count(loaded), node_count);
			let mut loaded_uuid_buf = [0u8; 64];
			let loaded_uuid_len =
				quercus_project_uuid(loaded, loaded_uuid_buf.as_mut_ptr(), loaded_uuid_buf.len());
			assert_eq!(&loaded_uuid_buf[..loaded_uuid_len as usize], uuid.as_bytes());
			let mut loaded_name_buf = [0u8; 64];
			let loaded_name_len =
				quercus_project_name(loaded, loaded_name_buf.as_mut_ptr(), loaded_name_buf.len());
			assert_eq!(&loaded_name_buf[..loaded_name_len as usize], name.as_bytes());
			let mut filename_buf = [0u8; 8];
			assert_eq!(
				quercus_project_filename(loaded, filename_buf.as_mut_ptr(), filename_buf.len()),
				0,
				"bytes carry no filename"
			);

			quercus_project_release(loaded);
			quercus_project_release(project);
			let _ = std::fs::remove_file(&path);
		}
	}

	#[test]
	fn load_from_bytes_reports_version_and_errors() {
		unsafe {
			let mut version_info = -1;
			let mut error = -1;

			// Newer document: null handle, TOO_NEW, no error.
			let too_new = b"<olive version=\"999999\"></olive>";
			assert!(quercus_project_load_from_bytes(
				too_new.as_ptr(),
				too_new.len(),
				&mut version_info,
				&mut error,
			)
			.is_null());
			assert_eq!(version_info, QUERCUSSTORAGE_TOO_NEW);
			assert_eq!(error, 0);

			// Unrecognized root: null handle, UNKNOWN_VERSION, no error.
			let unknown = b"<thirdparty></thirdparty>";
			assert!(quercus_project_load_from_bytes(
				unknown.as_ptr(),
				unknown.len(),
				&mut version_info,
				&mut error,
			)
			.is_null());
			assert_eq!(version_info, QUERCUSSTORAGE_UNKNOWN_VERSION);
			assert_eq!(error, 0);

			// Not XML: format error.
			let not_xml = b"not xml";
			assert!(quercus_project_load_from_bytes(
				not_xml.as_ptr(),
				not_xml.len(),
				&mut version_info,
				&mut error,
			)
			.is_null());
			assert_eq!(error, QUERCUSSTORAGE_E_FORMAT);

			// Non-UTF-8 bytes: format error.
			let non_utf8 = [0xFFu8, 0xFE, 0x00];
			assert!(quercus_project_load_from_bytes(
				non_utf8.as_ptr(),
				non_utf8.len(),
				&mut version_info,
				&mut error,
			)
			.is_null());
			assert_eq!(error, QUERCUSSTORAGE_E_FORMAT);

			// Null input pointer: invalid argument.
			assert!(
				quercus_project_load_from_bytes(
					std::ptr::null(),
					0,
					&mut version_info,
					&mut error
				)
				.is_null()
			);
			assert_eq!(error, QUERCUSSTORAGE_E_INVALID);

			// Null version-info pointer: invalid argument, reported
			// through the error slot.
			assert!(quercus_project_load_from_bytes(
				not_xml.as_ptr(),
				not_xml.len(),
				std::ptr::null_mut(),
				&mut error,
			)
			.is_null());
			assert_eq!(error, QUERCUSSTORAGE_E_INVALID);
		}
	}

	#[test]
	fn settings_write_syncs_the_pipeline() {
		let project = quercus_project_new();
		unsafe {
			let saved_working = crate::corestate::quercus_core_color_get_working_space;
			let mut buf = [0u8; 64];
			let n = saved_working(buf.as_mut_ptr(), buf.len());
			let saved = std::str::from_utf8(&buf[..n as usize]).unwrap().to_string();

			let (w, wl) = arg("srgb_legacy");
			assert!(quercus_project_set_working_color_space(project, w, wl));
			let n = crate::corestate::quercus_core_color_get_working_space(buf.as_mut_ptr(), buf.len());
			assert_eq!(
				std::str::from_utf8(&buf[..n as usize]).unwrap(),
				"srgb_legacy",
				"the pipeline followed the project setting"
			);

			// Restore the pipeline.
			assert!(quercus_project_set_working_color_space(project, saved.as_ptr(), saved.len()));
			quercus_project_release(project);
		}
	}

	#[test]
	fn deep_copy_is_independent() {
		let _guard = super::UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
		unsafe {
			let project = quercus_project_new();
			let (n, nl) = arg("CopyTest");
			let folder = quercus_folder_create(project, quercus_project_root(project), n, nl);
			assert!(quercus_node_id_is_valid(folder));

			let copy = quercus_project_deep_copy(project);
			assert!(!copy.is_null());
			assert_eq!(quercus_folder_child_count(copy, quercus_project_root(copy)), 1);

			// Mutating the copy leaves the original alone.
			assert!(quercus_node_remove(copy, folder));
			assert_eq!(quercus_folder_child_count(copy, quercus_project_root(copy)), 0);
			assert_eq!(quercus_folder_child_count(project, quercus_project_root(project)), 1);

			quercus_project_release(copy);
			quercus_project_release(project);
		}
	}

	#[test]
	fn project_settings_and_snapshot_field_queries() {
		unsafe {
			let project = quercus_project_new();
			let sequence = quercus_sequence_create(project, "Query".as_ptr(), 5);
			let list = quercus_sequence_track_list_at(project, sequence, 0);
			let track = quercus_tracklist_track_at(project, list, 0);
			let sequence_id = NodeId::from_identity(sequence).unwrap();
			let track_id = NodeId::from_identity(track).unwrap();
			with_project_mut(project, |guard| {
				let sequence_behavior = guard
					.graph
					.get_mut(sequence_id)
					.and_then(|entry| entry.behavior.as_any_mut())
					.and_then(|behavior| behavior.downcast_mut::<SequenceBehavior>())
					.expect("sequence behavior");
				sequence_behavior.video_params[0].interlaced = true;
				let track_behavior = guard
					.graph
					.get_mut(track_id)
					.and_then(|entry| entry.behavior.as_any_mut())
					.and_then(|behavior| behavior.downcast_mut::<TrackBehavior>())
					.expect("track behavior");
				track_behavior.height = 77.0;
				track_behavior.muted = true;
				track_behavior.locked = true;
			});
			assert_eq!(quercus_sequence_interlaced(project, sequence), 1);
			assert_eq!(quercus_track_height(project, track), 77.0);
			assert!(quercus_track_muted(project, track));
			assert!(quercus_track_locked(project, track));

			assert!(quercus_project_set_setting(project, "key".as_ptr(), 3, "value".as_ptr(), 5));
			let mut buf = [0u8; 64];
			let len = quercus_project_setting(project, "key".as_ptr(), 3, buf.as_mut_ptr(), buf.len());
			assert_eq!(std::str::from_utf8(&buf[..len as usize]).unwrap(), "value");
			assert!(!quercus_project_set_setting(project, "".as_ptr(), 0, "bad".as_ptr(), 3));
			assert!(quercus_project_set_cache_location(project, 2, "/tmp/cache".as_ptr(), 10));
			let mut mode = -1;
			let len = quercus_project_cache_location(project, &mut mode, buf.as_mut_ptr(), buf.len());
			assert_eq!(mode, 2);
			assert_eq!(std::str::from_utf8(&buf[..len as usize]).unwrap(), "/tmp/cache");

			let expected = with_project(project, |guard| guard.uuid.clone()).unwrap();
			let len = quercus_project_uuid(project, buf.as_mut_ptr(), buf.len());
			assert_eq!(std::str::from_utf8(&buf[..len as usize]).unwrap(), expected);
			quercus_project_release(project);
		}
	}
}
