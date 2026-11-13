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

//! OfxMessageSuite v1/v2: plugin messages -> host log.
//!
//! Stable Rust cannot define C-variadic functions (c_variadic is still unstable), so
//! v1's `...` and v2's va_list entry points are carried by a C shim
//! (cbits/ofx_message_shim.c, compiled by build.rs via `cc`; the symbols enter
//! libquercusplugin through the staticlib): the C side vsnprintf's into a fixed buffer and forwards
//! to this module's `quercus_ofx_message_impl`.
//!
//! The message sink is the facade-registered `quercusplugin_message_fn`
//! (include/plugin/host.h). This module models the header's contract.
//! (The original C ABI export layer was removed with the single-library change; the facade calls the registration point directly.)

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr};

use crate::suites::status;

/// Facade message callback (include/plugin/host.h `quercusplugin_message_fn`):
/// `(type, message, userdata) -> QUERCUSPLUGIN_MESSAGE_ANSWER_NO/YES`.
pub(crate) type MessageHandler =
	unsafe extern "C" fn(*const c_char, *const c_char, *mut c_void) -> c_int;

/// Message-sink registry (written by the facade, read by the suite; userdata is stored as a usize
/// to avoid a raw pointer breaking the static's Send/Sync inference).
static HANDLER: std::sync::Mutex<(Option<MessageHandler>, usize)> =
	std::sync::Mutex::new((None, 0));

// ---------------------------------------------------------------------------
// 持久消息计数（检查器效果卡的徽标数据源，阶段 6b）
// ---------------------------------------------------------------------------
//
// C++ 侧 `OlivePluginInstance::persistentErrors_` 按实例累计持久消息，
// 并 emit `node_->message_count_changed()`。这里以实例 handle（props
// 地址，见 [`crate::suites::tag`]）为键累计计数；app 检查器经
// [`crate::node_factory::instance_from_id`] 拿实例后按 props 地址查询。

static PERSISTENT: std::sync::OnceLock<std::sync::Mutex<HashMap<usize, usize>>> =
	std::sync::OnceLock::new();

fn persistent_slot() -> &'static std::sync::Mutex<HashMap<usize, usize>> {
	PERSISTENT.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// The persistent message count of one instance (the data source for the inspector effect card's badge).
///
/// Looked up by instance handle (the props address after removing the tag, per [`crate::suites::tag`]);
/// an instance that was never registered returns `0`.
///
/// # Arguments
///
/// * `handle` - the instance's props address (the bare handle with the tag stripped, see
///   [`crate::suites::tag::strip`]).
pub fn persistent_message_count(handle: usize) -> usize {
	persistent_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&handle)
		.copied()
		.unwrap_or(0)
}

/// Clears one instance's persistent message count and removes its entry (instance-release/host-shutdown path).
///
/// An unregistered `handle` is a no-op and does not create an entry; after clearing,
/// [`persistent_message_count`] returns `0` for it.
///
/// # Arguments
///
/// * `handle` - the instance's props address (tag already stripped, see
///   [`crate::suites::tag::strip`]).
pub fn clear_persistent_messages(handle: usize) {
	persistent_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.remove(&handle);
}

/// Registers or unregisters the plugin message sink (called by the facade; tests inject a capturer directly).
///
/// The sink receives the plugin's `(type, message, userdata)` and returns whether to answer "yes"
/// (the answer for `question`-type messages). Registration is overwriting: only the most recent one takes effect.
///
/// # Arguments
///
/// * `f` - the C ABI callback `(type, message, userdata) -> 0/1` (NO/YES);
///   `None` unregisters, after which messages go only to stderr.
/// * `userdata` - an opaque pointer passed back to `f` unchanged; may be null.
pub fn set_handler(f: Option<MessageHandler>, userdata: *mut c_void) {
	let mut h = HANDLER.lock().unwrap_or_else(|e| e.into_inner());
	*h = (f, userdata as usize);
}

/// Reads the current sink.
fn handler() -> (Option<MessageHandler>, *mut c_void) {
	let h = HANDLER.lock().unwrap_or_else(|e| e.into_inner());
	(h.0, h.1 as *mut c_void)
}

/// kOfxMessageQuestion (ofxMessage.h:61): answers "no" when there is no sink.
const K_MESSAGE_QUESTION: &[u8] = b"OfxMessageQuestion";

/// Returns whether the message type is OFX's `kOfxMessageQuestion`
/// (`"OfxMessageQuestion"`).
///
/// question-type messages require the host to return a "yes"/"no" answer to the plugin; other types ignore the handler's
/// return value. The comparison is byte-for-byte, case-sensitive equality.
///
/// # Arguments
///
/// * `type_` - a NUL-terminated message-type string; a null pointer returns `false` (call sites
///   normally reject that first).
fn is_question(type_: *const c_char) -> bool {
	// 空指针已在调用点拦截；此处仍防御（CStr::from_ptr 的调用方义务）。
	if type_.is_null() {
		return false;
	}
	unsafe { CStr::from_ptr(type_) }.to_bytes() == K_MESSAGE_QUESTION
}

/// Rust-side implementation (the C shim forwards here; `#[no_mangle]` keeps the symbol name identical to the C side's
/// reference). Semantics map to C++ clause by clause:
/// - an empty type/message -> kOfxStatFailed (olivehost.cpp:267, vmessage's
///   handling of !type || !format; the empty-format case is forwarded by the shim with a
///   NULL message);
/// - with a sink -> forwards `(type, message, userdata)`, and the return value is mapped
///   to REPLY_YES/REPLY_NO per the header contract (the handler path at olivehost.cpp:277-279;
///   plugins ignore the return value for non-question messages);
/// - no sink -> stderr log (mirroring olivehost.cpp:282's fprintf default),
///   answering "no" for questions (olivehost.cpp:283-284).
///
/// `# Safety`: `type`/`message` must be valid C strings (plugin contract;
/// the shim already guarantees `message` is not dangling — it points to its stack buffer, valid during the call).
#[no_mangle]
pub unsafe extern "C" fn quercus_ofx_message_impl(
	_handle: *mut c_void,
	type_: *const c_char,
	_id: *const c_char,
	message: *const c_char,
) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		if type_.is_null() || message.is_null() {
			return status::FAILED;
		}
		// 持久消息：按实例 handle（剥标签后的 props 地址）累计计数
		// （徽标数据源；C++ persistentErrors_ 的等价物）。
		if !_handle.is_null() {
			let key = crate::suites::tag::strip(_handle) as usize;
			let mut map = persistent_slot().lock().unwrap_or_else(|e| e.into_inner());
			*map.entry(key).or_insert(0) += 1;
			drop(map);
		}
		let (handler, userdata) = handler();
		if let Some(h) = handler {
			// 头文件契约：返回值 0/1 即答复 NO/YES。
			let answer = unsafe { h(type_, message, userdata) };
			return if answer != 0 {
				status::REPLY_YES
			} else {
				status::REPLY_NO
			};
		}
		// Headless 默认：stderr（C++ fprintf 的 Rust 等价物）。
		eprintln!(
			"OFX message: {}",
			unsafe { CStr::from_ptr(message) }.to_string_lossy()
		);
		if is_question(type_) {
			status::REPLY_NO
		} else {
			status::OK
		}
	}))
	.unwrap_or(status::FAILED)
}

/// Function-table layout (OfxMessageSuiteV1).
#[repr(C)]
pub struct MessageSuiteV1 {
	/// message (variadic version; the entry point is in the C shim)
	pub message: unsafe extern "C" fn(
		*mut c_void,
		*const c_char,
		*const c_char,
		*const c_char,
		...
	) -> c_int,
}

/// Function-table layout (OfxMessageSuiteV2: the 5th parameter is a platform-dependent `va_list`).
///
/// The real ABI is C's va_list (on Apple arm64 it is a struct passed by value);
/// declaring it as an opaque pointer is only a placeholder — this table is called only from the plugin to the C shim, the Rust side
/// never calls it directly, and the pointer type takes part in no actual call.
#[repr(C)]
pub struct MessageSuiteV2 {
	/// message (va_list version; the entry point is in the C shim)
	pub message: unsafe extern "C" fn(
		*mut c_void,
		*const c_char,
		*const c_char,
		*const c_char,
		*mut c_void,
	) -> c_int,
}

extern "C" {
	/// The C shim's v1 entry point (cbits/ofx_message_shim.c).
	fn ofx_message_shim_v1(
		handle: *mut c_void,
		type_: *const c_char,
		id: *const c_char,
		format: *const c_char,
		...
	) -> c_int;
	/// The C shim's v2 entry point (the va_list has already been consumed by the C side; Rust sees only an opaque value).
	fn ofx_message_shim_v2(
		handle: *mut c_void,
		type_: *const c_char,
		id: *const c_char,
		format: *const c_char,
		args: *mut c_void,
	) -> c_int;
}

/// Static function-table instance (v1).
pub fn suite_v1() -> &'static MessageSuiteV1 {
	static SUITE: std::sync::OnceLock<MessageSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| MessageSuiteV1 {
		message: ofx_message_shim_v1,
	})
}

/// Static function-table instance (v2).
pub fn suite_v2() -> &'static MessageSuiteV2 {
	static SUITE: std::sync::OnceLock<MessageSuiteV2> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| MessageSuiteV2 {
		message: ofx_message_shim_v2,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::CString;

	/// The tests share the global HANDLER and cargo runs them in parallel by default — serialize the contract cases.
	static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	/// Capturing sink: records (type, message) into the Vec pointed to by userdata.
	unsafe extern "C" fn capture(
		type_: *const c_char,
		message: *const c_char,
		userdata: *mut c_void,
	) -> c_int {
		let v = unsafe { &mut *(userdata as *mut Vec<(String, String)>) };
		v.push((
			unsafe { CStr::from_ptr(type_) }
				.to_string_lossy()
				.into_owned(),
			unsafe { CStr::from_ptr(message) }
				.to_string_lossy()
				.into_owned(),
		));
		1
	}

	/// v1 entry point: `...` is vsnprintf'd on the C shim side and forwarded; formatting is correct.
	#[test]
	fn shim_v1_formats_and_forwards() {
		let _g = TEST_LOCK.lock().unwrap();
		let mut captured: Vec<(String, String)> = Vec::new();
		set_handler(Some(capture), &mut captured as *mut _ as *mut c_void);

		let s = suite_v1();
		let type_ = CString::new("OfxMessageError").unwrap();
		let id = CString::new("test-id").unwrap();
		let fmt = CString::new("value=%d name=%s").unwrap();
		let name = CString::new("quercus").unwrap();
		let r = unsafe {
			(s.message)(
				std::ptr::null_mut(),
				type_.as_ptr(),
				id.as_ptr(),
				fmt.as_ptr(),
				42,
				name.as_ptr(),
			)
		};

		// 出口答 YES → REPLY_YES。
		assert_eq!(r, status::REPLY_YES);
		assert_eq!(captured.len(), 1);
		assert_eq!(captured[0].0, "OfxMessageError");
		assert_eq!(captured[0].1, "value=42 name=quercus");

		set_handler(None, std::ptr::null_mut());
	}

	/// v1 entry point: NULL format -> kOfxStatFailed (C++ olivehost.cpp:267).
	#[test]
	fn shim_v1_null_format_fails() {
		let _g = TEST_LOCK.lock().unwrap();
		let s = suite_v1();
		let type_ = CString::new("OfxMessageError").unwrap();
		let id = CString::new("id").unwrap();
		let r = unsafe {
			(s.message)(
				std::ptr::null_mut(),
				type_.as_ptr(),
				id.as_ptr(),
				std::ptr::null(),
			)
		};
		assert_eq!(r, status::FAILED);
	}

	/// v2 entry point: the table field is non-null and points to the C shim symbol (the va_list path cannot be called from
	/// Rust by constructing a va_list; the rest of the chain shares v1's forward).
	#[test]
	fn shim_v2_table_entry_resolves() {
		let _g = TEST_LOCK.lock().unwrap();
		let s = suite_v2();
		assert!(std::ptr::eq(
			s.message as *const (),
			ofx_message_shim_v2 as *const ()
		));
	}

	/// No sink (headless default): non-question -> OK; question -> REPLY_NO.
	#[test]
	fn no_handler_headless_defaults() {
		let _g = TEST_LOCK.lock().unwrap();
		set_handler(None, std::ptr::null_mut());
		let s = suite_v1();
		let type_ = CString::new("OfxMessageError").unwrap();
		let id = CString::new("id").unwrap();
		let fmt = CString::new("hello %s").unwrap();
		let arg = CString::new("x").unwrap();
		let r = unsafe {
			(s.message)(
				std::ptr::null_mut(),
				type_.as_ptr(),
				id.as_ptr(),
				fmt.as_ptr(),
				arg.as_ptr(),
			)
		};
		assert_eq!(r, status::OK);

		let q = CString::new("OfxMessageQuestion").unwrap();
		let r = unsafe {
			(s.message)(
				std::ptr::null_mut(),
				q.as_ptr(),
				id.as_ptr(),
				fmt.as_ptr(),
				arg.as_ptr(),
			)
		};
		assert_eq!(r, status::REPLY_NO);
	}

	/// Persistent messages accumulate per instance handle (the badge data source); clear removes the entry.
	#[test]
	fn persistent_messages_accumulate_per_handle() {
		let _g = TEST_LOCK.lock().unwrap();
		set_handler(None, std::ptr::null_mut());
		let props = crate::property::PropertySet::new();
		let handle = crate::suites::tag::make(&props, crate::suites::tag::INSTANCE);
		let key = crate::suites::tag::strip(handle) as usize;
		clear_persistent_messages(key);

		let type_ = CString::new("OfxMessageError").unwrap();
		let id = CString::new("id").unwrap();
		let msg = CString::new("boom").unwrap();
		unsafe {
			quercus_ofx_message_impl(handle, type_.as_ptr(), id.as_ptr(), msg.as_ptr());
			quercus_ofx_message_impl(handle, type_.as_ptr(), id.as_ptr(), msg.as_ptr());
		}
		assert_eq!(persistent_message_count(key), 2);
		clear_persistent_messages(key);
		assert_eq!(persistent_message_count(key), 0);
		// 空 handle 不计数。
		unsafe {
			quercus_ofx_message_impl(
				std::ptr::null_mut(),
				type_.as_ptr(),
				id.as_ptr(),
				msg.as_ptr(),
			);
		}
		assert_eq!(persistent_message_count(key), 0);
	}
}
