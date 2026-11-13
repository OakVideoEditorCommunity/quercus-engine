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

//! Codec task submitter registration, mirroring
//! `src/task/src/codecbridge.h`.
//!
//! Wires quercuscodec's task-submit callback (`quercus_codec::task::set_task_submit_cb`)
//! so conform/proxy requests from the codec side land back in the task
//! module. The registration is a direct Rust closure (single-lib
//! unification: the old extern-C submit callback is gone).
//!
//! CPP-PARITY: src/task/src/codecbridge.h

use quercus_codec::error::Error as CodecError;
use quercus_codec::proxymanager::ProxyManager;
use quercus_codec::task::{
	set_task_submit_cb, task_submit_is_registered, TaskKind, TaskRequest, TaskSubmitFn,
};

use crate::conform::ConformTask;
use crate::error::{Error, Result};
use crate::proxy::{ProxyParams, ProxyTask};
use crate::task::Task;

/// Copy codec-side proxy settings into the task module's [`ProxyParams`].
///
/// `params` is the codec's config struct ([`quercus_codec::proxymanager::ProxyParams`],
/// the C-compatible one with `int`/fixed `[u8; 32]` fields). The scalar
/// fields (`width`, `height`, `divider`, `version`, `crf`) are copied
/// verbatim — 0 width/height means "derive from `divider`", as usual for
/// the proxy settings — and all values are treated as already validated;
/// no clamping is done here. `include_audio` (a C `int`) becomes `true`
/// for any non-zero value. `extension` and `preset` are NUL-padded C
/// strings and are decoded with [`cstr_buf_to_string`].
fn proxy_params_from_codec(params: &quercus_codec::proxymanager::ProxyParams) -> ProxyParams {
	ProxyParams {
		width: params.width,
		height: params.height,
		divider: params.divider,
		version: params.version,
		crf: params.crf,
		include_audio: params.include_audio != 0,
		extension: cstr_buf_to_string(&params.extension),
		preset: cstr_buf_to_string(&params.preset),
	}
}

/// Decode one fixed-size C string buffer into a Rust [`String`].
///
/// `buf` is the raw byte array of a C `char[N]` field: reading stops at
/// the first `0` byte, so both NUL-terminated and NUL-padded contents work;
/// a buffer with no NUL byte is consumed in full. Bytes are interpreted as
/// UTF-8 lossily, so invalid sequences become `U+FFFD` rather than an
/// error — the result is always usable as a display/config value, never a
/// faithful byte copy. Used here for the `extension` and `preset` fields of
/// [`quercus_codec::proxymanager::ProxyParams`].
fn cstr_buf_to_string(buf: &[u8]) -> String {
	let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
	String::from_utf8_lossy(&buf[..len]).into_owned()
}

/// The installed submit callback: turn one codec-side request into a
/// conform or proxy task and run it to completion before returning.
///
/// Mirrors `submit_codec_task` in codecbridge.cpp, but the interim
/// contract is synchronous submission: the concrete task is wrapped in
/// an outer [`Task`] that shares the inner task's cancellation atom, so
/// cancelling the outer handle reaches the behavior, and
/// [`Task::start`] drives it on the calling thread — the call returns
/// only once the task has finished. `req.kind` selects the task:
/// [`TaskKind::Conform`] builds a [`ConformTask`] from the request,
/// [`TaskKind::Proxy`] reads the current proxy settings through
/// [`ProxyManager::proxy_params_from_config`], converts them with
/// [`proxy_params_from_codec`], and builds a [`ProxyTask`].
///
/// `_userdata` is accepted for the callback ABI but ignored (registration
/// always passes null).
///
/// # Errors
///
/// Returns `CodecError::Failed("conform task failed")` or
/// `CodecError::Failed("proxy task failed")` whenever the task's
/// `start()` fails; the underlying task error is discarded, so callers
/// only see the generic message.
fn submit_codec_task(
	req: &TaskRequest,
	_userdata: *mut std::ffi::c_void,
) -> quercus_codec::error::Result<()> {
	// Build the concrete task on an outer base so the behavior can be
	// driven by `start()`; both share the cancellation atom.
	match req.kind {
		TaskKind::Conform => {
			let task = ConformTask::new(req);
			let atom = task.base.get_cancel_atom();
			let title = task.base.title().to_string();
			let mut outer = Task::new(&title, Some(atom));
			outer.set_behavior(Box::new(task));
			outer
				.start()
				.map_err(|_| CodecError::Failed("conform task failed".to_string()))
		}
		TaskKind::Proxy => {
			let codec_params = ProxyManager::proxy_params_from_config();
			let params = proxy_params_from_codec(&codec_params);
			let task = ProxyTask::new(req, params);
			let atom = task.base.get_cancel_atom();
			let title = task.base.title().to_string();
			let mut outer = Task::new(&title, Some(atom));
			outer.set_behavior(Box::new(task));
			outer
				.start()
				.map_err(|_| CodecError::Failed("proxy task failed".to_string()))
		}
	}
}

/// Install the task-module submitter as quercuscodec's callback. Returns
/// `Err(Error::State)` if already registered.
pub fn register_codec_task_submitter() -> Result<()> {
	if is_codec_task_submitter_registered() {
		return Err(Error::State);
	}
	let cb: &'static TaskSubmitFn = Box::leak(Box::new(submit_codec_task));
	set_task_submit_cb(Some(cb), std::ptr::null_mut());
	Ok(())
}

/// Remove the task-module submitter from quercuscodec. Idempotent.
pub fn unregister_codec_task_submitter() -> Result<()> {
	set_task_submit_cb(None, std::ptr::null_mut());
	Ok(())
}

/// Whether a submitter is currently installed (queries quercuscodec).
pub fn is_codec_task_submitter_registered() -> bool {
	task_submit_is_registered()
}
