//! Session-scoped proxy generation, completion and status queries.
//!
//! Transcodes run in the engine task registry. The session owns the association
//! between a footage node and its active task, while this module is the only
//! worker-lifecycle path allowed to change proxy generation state in the graph.

use quercus_node::footage::FootageBehavior;
use quercus_node::id::NodeId;

use super::EditorSessionHandle;
use crate::codec::{copy_str_out, str_arg};

/// POD proxy metadata returned by [`quercus_editor_proxy_status`].
///
/// `state` uses the public codec proxy-state numbering: 0 missing, 1 generating,
/// 2 ready and 3 failed. A live session task takes precedence over disk state.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusEditorProxyStatus {
	pub state: i32,
	pub enabled: bool,
	pub has_custom_params: bool,
	pub can_generate: bool,
	pub has_proxy: bool,
}

/// Resolves `footage` to its `FootageBehavior`, or `None` when the id does not
/// name a footage node.
///
/// Used by the proxy mutations in this module to change generation state in
/// the project graph. The graph is borrowed mutably, so the returned reference
/// must not outlive the caller's borrow or be held across another project lock.
fn footage_mut(
	project: &mut quercus_node::project::Project,
	footage: NodeId,
) -> Option<&mut FootageBehavior> {
	project
		.graph
		.get_mut(footage)
		.and_then(|entry| entry.behavior.as_any_mut())
		.and_then(|behavior| behavior.downcast_mut::<FootageBehavior>())
}

/// Starts a proxy transcode without blocking the calling/UI thread.
///
/// On success the footage is immediately marked generating and the returned
/// nonzero task ID can be polled through `quercus_task_*`; the caller must
/// eventually call [`quercus_editor_proxy_complete`] with that same ID and
/// release the task. Duplicate generation for one footage in the same session
/// is rejected.
///
/// # Arguments
///
/// * `session` - live editor session handle.
/// * `footage` - identity of a footage node with a media file and a video
///   stream.
/// * `cache_path` - proxy cache directory (UTF-8); the final file name is
///   derived from it, the source filename, the stream index and the effective
///   proxy parameters.
/// * `cache_path_len` - byte length of `cache_path`.
/// * `error` - output buffer for a failure message; may be null with
///   `error_len == 0`. Written NUL-terminated and truncated like
///   `snprintf`.
/// * `error_len` - capacity of `error` in bytes.
///
/// Returns the nonzero task id on success, or 0 on failure (with a message in
/// `error` when provided).
///
/// # Safety
///
/// `session` must be null or a live handle; `cache_path` must be null or point
/// to `cache_path_len` readable bytes; `error` must be null or writable for
/// `error_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_proxy_generate(
	session: *const EditorSessionHandle,
	footage: u64,
	cache_path: *const u8,
	cache_path_len: usize,
	error: *mut u8,
	error_len: usize,
) -> u64 {
	let result = (|| {
		let session = unsafe { session.as_ref() }.ok_or("no editor session")?;
		let footage = NodeId::from_identity(footage).ok_or("invalid footage id")?;
		let cache_path =
			unsafe { str_arg(cache_path, cache_path_len) }.ok_or("invalid proxy cache path")?;
		let mut state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
		if state.proxy_tasks.contains_key(&footage.identity()) {
			return Err("a proxy is already generating for this footage");
		}
		let (filename, stream_index, params) = {
			let mut project = state.project.lock().unwrap_or_else(|e| e.into_inner());
			let footage_behavior =
				footage_mut(&mut project, footage).ok_or("entry is not footage")?;
			if footage_behavior.filename.is_empty() {
				return Err("the footage has no media file");
			}
			let stream_index = footage_behavior
				.streams
				.iter()
				.find(|stream| stream.is_video)
				.map(|stream| stream.index)
				.ok_or("the footage has no video stream")?;
			(
				footage_behavior.filename.clone(),
				stream_index,
				footage_behavior.effective_proxy_params(),
			)
		};
		let proxy_path = quercus_codec::proxymanager::ProxyManager::get_proxy_filename(
			cache_path,
			&filename,
			stream_index,
			&params,
		)
		.map_err(|_| "failed to build the proxy filename")?;
		let task_id = crate::task::spawn_proxy(&filename, &proxy_path, stream_index, &params);
		let mut project = state.project.lock().unwrap_or_else(|e| e.into_inner());
		let Some(footage_behavior) = footage_mut(&mut project, footage) else {
			drop(project);
			crate::task::quercus_task_cancel(task_id);
			crate::task::quercus_task_release(task_id);
			return Err("entry is not footage");
		};
		footage_behavior.set_proxy(&proxy_path, 1, stream_index, params.version, true);
		drop(project);
		state.proxy_tasks.insert(footage.identity(), task_id);
		Ok(task_id)
	})();
	match result {
		Ok(task_id) => task_id,
		Err(message) => {
			copy_str_out(message, error, error_len);
			0
		}
	}
}

/// Applies the terminal state of a previously started proxy task.
///
/// On success the footage's proxy state becomes ready (2) when `succeeded` is
/// true, or failed (3) otherwise; a success also enables the proxy. Returns
/// false when the footage/task pair is not the active generation, which guards
/// against an old worker overwriting metadata for a newer request.
///
/// # Arguments
///
/// * `session` - live editor session handle.
/// * `footage` - identity of the footage the task was started for.
/// * `task_id` - nonzero task id returned by [`quercus_editor_proxy_generate`].
/// * `succeeded` - whether the transcode finished without error.
///
/// # Safety
///
/// `session` must be null or a live handle from `quercus_editor_*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_proxy_complete(
	session: *const EditorSessionHandle,
	footage: u64,
	task_id: u64,
	succeeded: bool,
) -> bool {
	let Some(session) = (unsafe { session.as_ref() }) else {
		return false;
	};
	let Some(footage) = NodeId::from_identity(footage) else {
		return false;
	};
	let mut state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
	if state.proxy_tasks.get(&footage.identity()) != Some(&task_id) {
		return false;
	}
	state.proxy_tasks.remove(&footage.identity());
	let mut project = state.project.lock().unwrap_or_else(|e| e.into_inner());
	if let Some(footage) = footage_mut(&mut project, footage) {
		footage.proxy_state = if succeeded { 2 } else { 3 };
		if succeeded {
			footage.proxy_enabled = true;
		}
	}
	true
}

/// Reads one footage's proxy status into `out`.
///
/// An active session task takes priority over the on-disk working file (which
/// may have been left behind by an interrupted process), then a previously
/// failed generation is reported. The returned `state` uses the public codec
/// proxy-state numbering: 0 missing, 1 generating, 2 ready, 3 failed.
///
/// # Arguments
///
/// * `session` - live editor session handle.
/// * `footage` - identity of a footage node.
/// * `out` - receives the status; must be writable for one
///   `QuercusEditorProxyStatus`.
///
/// Returns false when any pointer/id is invalid or the node is not footage.
///
/// # Safety
///
/// `session` must be null or a live handle; `out` must be null or writable for
/// one `QuercusEditorProxyStatus`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_proxy_status(
	session: *const EditorSessionHandle,
	footage: u64,
	out: *mut QuercusEditorProxyStatus,
) -> bool {
	let (Some(session), Some(footage), Some(out)) = (
		unsafe { session.as_ref() },
		NodeId::from_identity(footage),
		unsafe { out.as_mut() },
	) else {
		return false;
	};
	let state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
	let project = state.project.lock().unwrap_or_else(|e| e.into_inner());
	let Some(footage_behavior) = project
		.graph
		.get(footage)
		.and_then(|entry| entry.behavior.as_any())
		.and_then(|behavior| behavior.downcast_ref::<FootageBehavior>())
	else {
		return false;
	};
	let disk_state = if footage_behavior.proxy.is_empty() {
		quercus_codec::proxymanager::ProxyState::Missing
	} else {
		quercus_codec::proxymanager::ProxyManager::get_proxy_state(&footage_behavior.proxy)
	};
	let proxy_state = if state.proxy_tasks.contains_key(&footage.identity()) {
		quercus_codec::proxymanager::ProxyState::Generating
	} else {
		match disk_state {
			quercus_codec::proxymanager::ProxyState::Ready => {
				quercus_codec::proxymanager::ProxyState::Ready
			}
			quercus_codec::proxymanager::ProxyState::Generating => {
				quercus_codec::proxymanager::ProxyState::Generating
			}
			_ if footage_behavior.proxy_state == 3 => quercus_codec::proxymanager::ProxyState::Failed,
			_ => quercus_codec::proxymanager::ProxyState::Missing,
		}
	};
	*out = QuercusEditorProxyStatus {
		state: proxy_state as i32,
		enabled: footage_behavior.proxy_enabled,
		has_custom_params: footage_behavior.has_custom_proxy_params(),
		// Generation only needs a media file and a video stream; `valid` is
		// the C++ probe flag and can be false for a project loaded without a
		// probe (or while a deleted proxy is being reset). The generate ABI
		// checks exactly these two facts, so gating the menu on `valid` made
		// Generate permanently gray for otherwise usable footage.
		can_generate: !footage_behavior.filename.is_empty()
			&& footage_behavior
				.streams
				.iter()
				.any(|stream| stream.is_video),
		has_proxy: !footage_behavior.proxy.is_empty(),
	};
	true
}

/// Copies the footage's recorded proxy path into `buf`, snprintf-style.
///
/// The path is returned even when the file is missing on disk (the caller
/// decides whether to regenerate). Returns the full byte length of the path
/// (excluding the NUL), or `-1` for a null session, an invalid footage id, or
/// a node that is not footage.
///
/// # Arguments
///
/// * `session` - live editor session handle.
/// * `footage` - identity of a footage node.
/// * `buf` - output buffer; null with `buf_len == 0` queries the required
///   length. Otherwise written NUL-terminated and truncated to `buf_len - 1`
///   bytes when too small.
/// * `buf_len` - capacity of `buf` in bytes.
///
/// # Safety
///
/// `session` must be null or a live handle; `buf` must be null or writable for
/// `buf_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_editor_proxy_path(
	session: *const EditorSessionHandle,
	footage: u64,
	buf: *mut u8,
	buf_len: usize,
) -> i32 {
	let (Some(session), Some(footage)) =
		(unsafe { session.as_ref() }, NodeId::from_identity(footage))
	else {
		return -1;
	};
	let state = session.inner.lock().unwrap_or_else(|e| e.into_inner());
	let project = state.project.lock().unwrap_or_else(|e| e.into_inner());
	project
		.graph
		.get(footage)
		.and_then(|entry| entry.behavior.as_any())
		.and_then(|behavior| behavior.downcast_ref::<FootageBehavior>())
		.map(|footage| copy_str_out(&footage.proxy, buf, buf_len))
		.unwrap_or(-1)
}
