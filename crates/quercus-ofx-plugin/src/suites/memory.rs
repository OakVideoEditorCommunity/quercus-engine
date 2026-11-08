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

//! OfxMemorySuite v1: allocation on the host's behalf.
//!
//! Semantics follow HS: ofxhHost.cpp:26-42 (plain malloc/free semantics:
//! failure → kOfxStatErrMemory, free always OK) — but this crate adds a
//! **host-level ledger** on top of the skeleton declaration: allocations are
//! recorded in the ledger and reclaimed as a backstop on destroyInstance (a leak
//! guard; plugins are expected to free normally). Freeing an unknown pointer →
//! kOfxStatErrBadHandle (the contract in SDK ofxMemory.h:51).
//! The image_effect suite's imageMemory* functions share this ledger.

use std::alloc::Layout;
use std::sync::Mutex;

use crate::suites::status;

/// Alignment: malloc guarantees max_align_t (16 bytes on this platform); plugins
/// use the buffer as float*/SIMD, and 16 bytes is the safe lower bound.
const ALIGN: usize = 16;

/// One allocated block: address + layout (reclaimed with the recorded layout).
/// The address is stored as `usize` (an opaque token, so raw pointers do not break
/// `static`'s Send/Sync inference; the ledger only pairs alloc/dealloc and never
/// dereferences).
struct Block {
	ptr: usize,
	layout: Layout,
}

/// Host-level ledger (process singleton; the memory suite has no host pointer argument, so global is fine).
static LEDGER: Mutex<Vec<Block>> = Mutex::new(Vec::new());

/// Take the lock (poisoning is adopted: one panic while holding it does not cascade).
fn lock() -> std::sync::MutexGuard<'static, Vec<Block>> {
	LEDGER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Allocate and record. Returns a raw pointer; `None` on failure (OOM/invalid layout).
pub(crate) fn alloc(size: usize) -> Option<*mut u8> {
	// 零尺寸分配：Layout 要求非零，取 1 字节兜底（free 按同布局回收）。
	let layout = Layout::from_size_align(size.max(1), ALIGN).ok()?;
	let ptr = unsafe { std::alloc::alloc(layout) };
	if ptr.is_null() {
		return None;
	}
	lock().push(Block {
		ptr: ptr as usize,
		layout,
	});
	Some(ptr)
}

/// Free by pointer and drop the ledger entry. An unknown pointer returns `false`
/// (the suite layer maps it to kOfxStatErrBadHandle); `NULL` is a no-op per C
/// semantics and returns `true`.
pub(crate) fn free(ptr: *mut u8) -> bool {
	if ptr.is_null() {
		return true;
	}
	let mut ledger = lock();
	let pos = ledger.iter().position(|b| b.ptr == ptr as usize);
	match pos {
		Some(i) => {
			let b = ledger.remove(i);
			unsafe { std::alloc::dealloc(b.ptr as *mut u8, b.layout) };
			true
		}
		None => false,
	}
}

/// Function-table layout (identical to the SDK `OfxMemorySuiteV1`; `size_t` is the
/// same width as `usize` on this platform, so stable Rust uses `usize` — the
/// skeleton's `c_size_t` is an unstable feature and is not used).
#[repr(C)]
pub struct MemorySuiteV1 {
	/// memoryAlloc: allocate `size` bytes and write `*out`.
	pub alloc: unsafe extern "C" fn(*mut c_void, usize, *mut *mut c_void) -> c_int,
	/// memoryFree: free; NULL is a no-op.
	pub free: unsafe extern "C" fn(*mut c_void) -> c_int,
}

use std::ffi::{c_int, c_void};

/// memoryAlloc implementation: `handle` is ignored (same as HS, ofxhHost.cpp:27).
///
/// `# Safety`: `out` must be a writable pointer (plugin contract).
unsafe extern "C" fn memory_alloc(
	_handle: *mut c_void,
	byte_size: usize,
	out: *mut *mut c_void,
) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		if out.is_null() {
			return status::ERR_VALUE;
		}
		match alloc(byte_size) {
			Some(ptr) => {
				unsafe { *out = ptr as *mut c_void };
				status::OK
			}
			// 与 HS 一致：分配失败 → kOfxStatErrMemory（ofxhHost.cpp:35）。
			None => status::ERR_MEMORY,
		}
	}))
	.unwrap_or(status::FAILED)
}

/// memoryFree implementation: an unknown pointer → kOfxStatErrBadHandle (SDK
/// contract); NULL is a no-op per C semantics → OK (HS's free(NULL) behaviour,
/// ofxhHost.cpp:40).
///
/// `data` is a pointer returned by this suite's [`memory_alloc`], or NULL. Return
/// values: freed successfully / NULL → `kOfxStatOK`; the pointer is not in the
/// host allocation table → `kOfxStatErrBadHandle`; an internal panic is caught and
/// reported as `kOfxStatFailed`.
/// The same pointer must not be freed twice.
///
/// # Safety
///
/// `data` must be a pointer allocated by this host's [`memory_alloc`] and not yet
/// freed (or NULL): after freeing it must not be dereferenced or freed again.
/// Passing a pointer the plugin malloc'd itself, one from another host, or an
/// already-freed one does not cause UB (the ledger only compares addresses and
/// never dereferences), but yields `kOfxStatErrBadHandle`.
/// The call may be made from any thread (the ledger is protected by a mutex).
unsafe extern "C" fn memory_free(data: *mut c_void) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		if free(data as *mut u8) {
			status::OK
		} else {
			status::ERR_BAD_HANDLE
		}
	}))
	.unwrap_or(status::FAILED)
}

/// The static function-table instance.
pub fn suite_v1() -> &'static MemorySuiteV1 {
	static SUITE: std::sync::OnceLock<MemorySuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| MemorySuiteV1 {
		alloc: memory_alloc,
		free: memory_free,
	})
}
