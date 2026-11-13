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

//! OfxMultiThreadSuite v1: registration and index assignment for plugin-spawned threads.
//!
//! Discipline: plugin threads may call back into any suite while this suite is
//! alive — all shared state must be Mutex-protected (README §3). Threads are
//! started with `std::thread::scope` and joined synchronously (OFX semantics:
//! when multiThread returns all threads have finished;
//! HS: ofxhImageEffect.cpp gMultiThreadSuite), so no persistent thread table is
//! needed.
//! Thread indices are assigned through TLS: the host thread gets [`index`] -1
//! and [`is_spawned`] 0.
//!
//! Reference: HS: ofxhImageEffect.cpp gMultiThreadSuite.

use std::collections::HashMap;
use std::ffi::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::suites::status;

thread_local! {
	/// This thread's OFX thread index (host thread = None).
	static THREAD_INDEX: std::cell::Cell<Option<c_uint>> = const { std::cell::Cell::new(None) };
}

/// Function-table layout (OfxMultiThreadSuiteV1).
#[repr(C)]
pub struct MultiThreadSuiteV1 {
	/// multiThread: start `n_threads` threads running `func`, with thread indices
	/// 0..n_threads-1 passed through `thread_arg`.
	pub multi_thread: unsafe extern "C" fn(
		func: unsafe extern "C" fn(c_uint, c_uint, *mut c_void),
		n_threads: c_uint,
		thread_arg: *mut c_void,
	) -> c_int,
	/// multiThreadNumCPUs
	pub num_cpus: unsafe extern "C" fn(*mut c_int) -> c_int,
	/// multiThreadIndex: the current thread's OFX thread index (host thread is -1).
	pub index: unsafe extern "C" fn(*mut c_int) -> c_int,
	/// multiThreadIsSpawnedThread: whether the current thread is a plugin thread.
	pub is_spawned: unsafe extern "C" fn(*mut c_int) -> c_int,
	/// mutexCreate: `count` is the initial available count (>1 means semaphore semantics).
	pub mutex_create: unsafe extern "C" fn(*mut *mut c_void, c_int) -> c_int,
	/// mutexDestroy
	pub mutex_destroy: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// mutexLock
	pub mutex_lock: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// mutexUnLock
	pub mutex_unlock: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// mutexTryLock
	pub mutex_try_lock: unsafe extern "C" fn(*mut c_void) -> c_int,
}

/// Common entry template: wrap a possibly-panicking closure into an OFX status code.
///
/// The closure returns the status code directly; a panic → `status::FAILED`
/// (panics must not cross the C ABI boundary). The closure has no separate error
/// channel; failures are always expressed by the return code.
fn caught(f: impl FnOnce() -> c_int) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(status::FAILED)
}

/// multiThread: synchronously start `n_threads` worker threads and wait for all of them.
///
/// One thread is started per index `0..n-1`; the thread body does only two
/// things: publish the TLS index, then call
/// `func(index, n_threads, thread_arg)` (a C callback that does not panic per the
/// OFX contract), clearing the TLS index on exit; after all joins it returns
/// `status::OK`. `thread_arg` is passed through unchanged to every call (carried
/// as `usize`, because raw pointers are not `Send`). `n_threads = 0` is a legal
/// no-op. A panic inside a thread is caught by `caught` and reported as
/// `status::FAILED`.
///
/// # Safety
///
/// `func` must be a valid C function pointer; the memory `thread_arg` points to
/// must stay valid for the whole callback and must allow concurrent access from
/// `n_threads` threads (the plugin is responsible for synchronisation).
unsafe extern "C" fn multi_thread(
	func: unsafe extern "C" fn(c_uint, c_uint, *mut c_void),
	n_threads: c_uint,
	thread_arg: *mut c_void,
) -> c_int {
	caught(|| {
		if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
			eprintln!("[ofx] multiThread: n_threads={n_threads} func={:#x} arg={thread_arg:p}", func as usize);
		}
		// 裸指针不可 Send：以 usize 搬运，线程内还原（C 侧本即
		// 整数传递语义）。
		let arg = thread_arg as usize;
		std::thread::scope(|scope| {
			for i in 0..n_threads {
				scope.spawn(move || {
					THREAD_INDEX.with(|t| t.set(Some(i)));
					// func 是插件代码：按 OFX 契约不 panic；Rust 侧
					// 只做 TLS 登记，无 panic 源。
					unsafe { (func)(i, n_threads, arg as *mut c_void) };
					THREAD_INDEX.with(|t| t.set(None));
				});
			}
		});
		status::OK
	})
}

/// multiThreadNumCPUs: write the number of CPUs available for rendering.
///
/// Uses `std::thread::available_parallelism()` (logical core count) with a
/// fallback of 1; a null `out` → `status::ERR_VALUE`. Plugins use it to pick the
/// thread count for `multi_thread`; it reflects the cores available to the
/// process, not the physical package.
///
/// # Safety
///
/// `out` must point to a writable `c_int`.
unsafe extern "C" fn multi_thread_num_cpus(out: *mut c_int) -> c_int {
	caught(|| {
		if out.is_null() {
			return status::ERR_VALUE;
		}
		// 物理核数（HS 同：系统 CPU 数）。
		let n = std::thread::available_parallelism()
			.map(|n| n.get() as c_int)
			.unwrap_or(1);
		unsafe { *out = n };
		status::OK
	})
}

/// multiThreadIndex: write the current worker thread's index (0..n).
///
/// Inside a thread started by `multi_thread` it writes that thread's index; on
/// the host thread or any thread outside the pool it writes `-1` (HS convention).
/// A null `out` → `status::ERR_VALUE`. Pairs with `multi_thread`; plugins use it
/// to partition pixel regions.
///
/// # Safety
///
/// `out` must point to a writable `c_int`.
unsafe extern "C" fn multi_thread_index(out: *mut c_int) -> c_int {
	caught(|| {
		if out.is_null() {
			return status::ERR_VALUE;
		}
		// 宿主线程 → -1（HS 约定）。
		unsafe { *out = THREAD_INDEX.with(|t| t.get()).map_or(-1, |i| i as c_int) };
		status::OK
	})
}

/// multiThreadIsSpawned: report whether the current thread is a worker started by `multi_thread`.
///
/// Writes 1 inside a worker thread and 0 on any other thread (the host render
/// thread, external threads); a null `out` → `status::ERR_VALUE`. Plugins use it
/// to decide whether thread-local state can safely be assumed initialised.
///
/// # Safety
///
/// `out` must point to a writable `c_int`.
unsafe extern "C" fn multi_thread_is_spawned(out: *mut c_int) -> c_int {
	caught(|| {
		if out.is_null() {
			return status::ERR_VALUE;
		}
		unsafe { *out = THREAD_INDEX.with(|t| t.get().is_some()) as c_int };
		status::OK
	})
}

// ---- 互斥锁（OfxMultiThreadSuiteV1 的 mutex* 函数组）-----------------------
//
// 句柄表：全局注册表按 usize 键发号；`count > 1` 是信号量语义（ofxMultiThread.h
// "a mutex with a count greater than 1 is a counting semaphore"）。
// 插件在多线程渲染期用它们保护共享状态——ofxs 支持库的
// ofxsThreadSuiteCheck 在 load 时逐一检查这些函数非空，缺一个整批插件
// 直接拒载。

/// Counting semaphore (a plain mutex when count==1).
struct OfxMutex {
	permits: std::sync::Mutex<c_int>,
	released: std::sync::Condvar,
}

/// Handle registry (the handle is the next incrementing id cast to a pointer; 0 is reserved for empty).
static MUTEXES: std::sync::LazyLock<Mutex<HashMap<usize, Arc<OfxMutex>>>> =
	std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// The next mutex id.
static NEXT_MUTEX: AtomicUsize = AtomicUsize::new(1);

fn mutex_lookup(handle: *mut c_void) -> Option<Arc<OfxMutex>> {
	if handle.is_null() {
		return None;
	}
	MUTEXES
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&(handle as usize))
		.cloned()
}

/// mutexCreate: create a mutex or counting semaphore and write its handle to `out`.
///
/// `count` is the initial available count: 1 = plain mutex, >1 = counting
/// semaphore (see ofxMultiThread.h); values below 1 are treated as 1. The handle
/// is an incrementing id issued by the global registry, with 0 reserved for the
/// null pointer. A null `out` → `status::ERR_VALUE`; a successfully created
/// handle must be returned through `mutex_destroy`.
///
/// # Safety
///
/// `out` must point to a writable `*mut c_void`.
unsafe extern "C" fn mutex_create(out: *mut *mut c_void, count: c_int) -> c_int {
	caught(|| {
		if out.is_null() {
			return status::ERR_VALUE;
		}
		let id = NEXT_MUTEX.fetch_add(1, Ordering::Relaxed);
		MUTEXES
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.insert(id, Arc::new(OfxMutex {
				permits: std::sync::Mutex::new(count.max(1)),
				released: std::sync::Condvar::new(),
			}));
		unsafe { *out = id as *mut c_void };
		status::OK
	})
}

/// mutexDestroy: unregister the handle and release the lock object.
///
/// Removes it from the global handle table; an unknown or null handle →
/// `status::ERR_BAD_HANDLE`. `Arc`s held by waiters keep the object alive until
/// they are woken, so destroying a lock that still has blocked threads does not
/// free the memory immediately, but the caller must still guarantee that no
/// thread uses the handle afterwards.
///
/// # Safety
///
/// `handle` must be a handle returned by `mutex_create`; it must not be passed to
/// any mutex function after destruction.
unsafe extern "C" fn mutex_destroy(handle: *mut c_void) -> c_int {
	caught(|| {
		if mutex_lookup(handle).is_none() {
			return status::ERR_BAD_HANDLE;
		}
		MUTEXES
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.remove(&(handle as usize));
		status::OK
	})
}

/// mutexLock: take the lock; block while the available count is exhausted.
///
/// With `count > 1` this is a counting semaphore (see ofxMultiThread.h), and
/// taking it decrements the available count; at 0 it sleeps on the condition
/// variable until `mutex_unlock` wakes it. An unknown or null handle →
/// `status::ERR_BAD_HANDLE`, without blocking. May be called from any thread.
///
/// # Safety
///
/// `handle` must be a handle returned by `mutex_create` and not yet destroyed; no
/// thread may still be blocked on the handle before destruction.
unsafe extern "C" fn mutex_lock(handle: *mut c_void) -> c_int {
	caught(|| {
		let Some(m) = mutex_lookup(handle) else {
			return status::ERR_BAD_HANDLE;
		};
		let mut permits = m.permits.lock().unwrap_or_else(|e| e.into_inner());
		while *permits <= 0 {
			permits = m.released.wait(permits).unwrap_or_else(|e| e.into_inner());
		}
		*permits -= 1;
		status::OK
	})
}

/// mutexUnlock: release one permit and wake one waiter.
///
/// Increments the available count, then calls `notify_one`; for a plain mutex
/// (count=1) that is the unlock. An unknown or null handle →
/// `status::ERR_BAD_HANDLE`. Over-release (beyond the initial `count`) is not
/// detected and pushes the count past its initial value. Pairs with
/// `mutex_lock`.
///
/// # Safety
///
/// `handle` must be a handle returned by `mutex_create` and not yet destroyed, and
/// the caller must actually hold one permit.
unsafe extern "C" fn mutex_unlock(handle: *mut c_void) -> c_int {
	caught(|| {
		let Some(m) = mutex_lookup(handle) else {
			return status::ERR_BAD_HANDLE;
		};
		{
			let mut permits = m.permits.lock().unwrap_or_else(|e| e.into_inner());
			*permits += 1;
		}
		m.released.notify_one();
		status::OK
	})
}

/// mutexTryLock: try to take the lock without blocking.
///
/// Cannot take it (available count is 0) → `status::FAILED`, which is the normal
/// OFX response rather than an error; took it → `status::OK` and the available
/// count is decremented. An unknown or null handle → `status::ERR_BAD_HANDLE`.
/// Semantics for a counting semaphore are the same as `mutex_lock`.
///
/// # Safety
///
/// `handle` must be a handle returned by `mutex_create` and not yet destroyed, or
/// a null pointer (→ `ERR_BAD_HANDLE`).
unsafe extern "C" fn mutex_try_lock(handle: *mut c_void) -> c_int {
	caught(|| {
		let Some(m) = mutex_lookup(handle) else {
			return status::ERR_BAD_HANDLE;
		};
		let mut permits = m.permits.lock().unwrap_or_else(|e| e.into_inner());
		if *permits <= 0 {
			// 规范：占不到锁返回 kOfxStatFailed（不是错误）。
			return status::FAILED;
		}
		*permits -= 1;
		status::OK
	})
}

/// The static function-table instance.
pub fn suite_v1() -> &'static MultiThreadSuiteV1 {
	static SUITE: std::sync::OnceLock<MultiThreadSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| MultiThreadSuiteV1 {
		multi_thread,
		num_cpus: multi_thread_num_cpus,
		index: multi_thread_index,
		is_spawned: multi_thread_is_spawned,
		mutex_create,
		mutex_destroy,
		mutex_lock,
		mutex_unlock,
		mutex_try_lock,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::{AtomicUsize, Ordering};

	/// Each plugin thread accumulates (index, count) into the userdata.
	unsafe extern "C" fn worker(index: c_uint, count: c_uint, arg: *mut c_void) {
		let state = unsafe { &mut *(arg as *mut (AtomicUsize, AtomicUsize)) };
		state.0.fetch_add(index as usize, Ordering::Relaxed);
		state.1.fetch_add(count as usize, Ordering::Relaxed);
	}

	#[test]
	fn multi_thread_spawns_and_joins() {
		let s = suite_v1();
		let state = (AtomicUsize::new(0), AtomicUsize::new(0));
		unsafe {
			assert_eq!(
				(s.multi_thread)(worker, 8, &state as *const _ as *mut c_void),
				status::OK
			);
		}
		// 索引 0..7 各一次 → 28；count 各 8 → 64。
		assert_eq!(state.0.load(Ordering::Relaxed), 28);
		assert_eq!(state.1.load(Ordering::Relaxed), 64);

		// 0 线程：no-op OK。
		unsafe {
			assert_eq!(
				(s.multi_thread)(worker, 0, &state as *const _ as *mut c_void),
				status::OK
			);
		}
	}

	/// Query index/isSpawned inside a plugin thread and write the result back into
	/// the slot `arg` points to. (A closure capturing the suite cannot be converted to
	/// an fn pointer, hence a named function plus a static slot.)
	static SPAWNED_RESULT: std::sync::LazyLock<std::sync::Mutex<Option<(c_int, c_int)>>> =
		std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

	unsafe extern "C" fn query_worker(_index: c_uint, _count: c_uint, _arg: *mut c_void) {
		let s = suite_v1();
		let mut i = 0;
		let mut sp = 1;
		unsafe {
			let _ = (s.index)(&mut i);
			let _ = (s.is_spawned)(&mut sp);
		}
		*SPAWNED_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some((i, sp));
	}

	#[test]
	fn index_and_spawned() {
		let s = suite_v1();
		let mut i = 0;
		let mut spawned = 1;
		unsafe {
			// 宿主线程：index = -1，isSpawned = 0。
			assert_eq!((s.index)(&mut i), status::OK);
			assert_eq!(i, -1);
			assert_eq!((s.is_spawned)(&mut spawned), status::OK);
			assert_eq!(spawned, 0);

			// 插件线程内：index 正确、isSpawned = 1。
			assert_eq!(
				(s.multi_thread)(query_worker, 1, std::ptr::null_mut()),
				status::OK
			);
			assert_eq!(
				*SPAWNED_RESULT.lock().unwrap_or_else(|e| e.into_inner()),
				Some((0, 1))
			);
		}
	}

	#[test]
	fn num_cpus_positive() {
		let s = suite_v1();
		let mut n = 0;
		unsafe {
			assert_eq!((s.num_cpus)(&mut n), status::OK);
		}
		assert!(n >= 1);
	}
}
