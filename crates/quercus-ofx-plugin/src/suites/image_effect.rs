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

//! OfxImageEffectSuite v1: clip/image operations.
//!
//! Semantics mirror HS: ofxhImageEffect.cpp:
//! - clipGetImage returns the image **property set** handle (HS:2038 `getPropHandle`);
//!   image lifetime is managed by the host's table (get -> insert a strong reference, clipReleaseImage -> remove
//!   and release; this mirrors HS's reference-count pairing, HS:2044);
//! - clipGetRegionOfDefinition: reads the clip instance property
//!   kOfxImageEffectPropRegionOfDefinition (written by the host during negotiation); an invalid
//!   RoD -> kOfxStatFailed (HS:2143-2147);
//! - abort: instance phase -> the cancellation state of the current render (HS:2154-2170; the contract is
//!   0 = continue, non-zero = abort — REPLY_YES/NO makes ofxs's
//!   `abort(...) != 0` check misread it as abort, so the pixel loop writes nothing -> black frame);
//! - imageMemory*: the ledger is the same as the memory suite (HS's lock means "lock against reallocation";
//!   the phase-1 ledger does not need it -> OK no-op, see the memory.rs docs).
//!
//! `// TODO(clip)`: clipGetImage depends on [`crate::clip::ClipInstance::fetch_image`]
//! (quercusrender frame access became a local stub with the single-library change, see [`crate::render`]); the code is complete, runtime waits on clip.rs.

use std::collections::HashMap;
use std::ffi::{c_char, c_double, c_int, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

use crate::clip::ClipInstance;
use crate::descriptor::{ClipDescriptor, EffectDescriptor};
use crate::image::Image;
use crate::instance::{Instance, OfxRectD, RenderScale};
use crate::property::PropertySet;
use crate::suites::{status, tag};

/// Function-table layout (field-for-field identical to the SDK's `OfxImageEffectSuiteV1`; the commonly used subset is
/// documented, the full field set follows the SDK).
#[repr(C)]
pub struct ImageEffectSuiteV1 {
	/// getPropertySet: gets the effect/clip/image property set.
	pub get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
	/// getParamSet: gets the instance param set.
	pub get_param_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
	/// clipDefine (describe phase): defines a clip.
	pub clip_define: unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> c_int,
	/// clipGetHandle: gets a clip by name.
	pub clip_get_handle: unsafe extern "C" fn(
		*mut c_void,
		*const c_char,
		*mut *mut c_void,
		*mut *mut c_void,
	) -> c_int,
	/// clipGetPropertySet
	pub clip_get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
	/// clipGetImage: gets an image (the host side adds a reference, which must be
	/// paired with clipReleaseImage).
	pub clip_get_image:
		unsafe extern "C" fn(*mut c_void, c_double, *const c_void, *mut *mut c_void) -> c_int,
	/// clipReleaseImage
	pub clip_release_image: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// clipGetRegionOfDefinition
	pub clip_get_region_of_definition:
		unsafe extern "C" fn(*mut c_void, c_double, *mut c_void) -> c_int,
	/// abort: queries whether to abort (0 = continue, non-zero = abort; cancellation is passed through).
	pub abort: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// imageMemoryAlloc / imageMemoryFree / imageMemoryLock /
	/// imageMemoryUnlock: image memory management (ledger same as the memory suite).
	pub image_memory_alloc: unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_void) -> c_int,
	/// imageMemoryFree
	pub image_memory_free: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// imageMemoryLock
	pub image_memory_lock: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
	/// imageMemoryUnlock
	pub image_memory_unlock: unsafe extern "C" fn(*mut c_void) -> c_int,
}

/// Live image table: produced by clipGetImage (props address -> strong reference; the sole owner,
/// clipReleaseImage removes it and thereby releases — mirroring HS's get/release pairing).
static LIVE_IMAGES: std::sync::LazyLock<Mutex<HashMap<usize, std::sync::Arc<Image>>>> =
	std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Common entry template: wraps a possibly panicking closure into an OFX status code.
///
/// The closure returns `Ok(())` -> `status::OK`; returns `Err(code)` -> that status code
/// is passed through unchanged; panic -> `status::FAILED` (a panic must not cross the C ABI boundary). Setting
/// the `QUERCUS_OFX_TRACE` environment variable prints the error location and the result of every call to
/// stderr; the location comes from the call site captured by `#[track_caller]`.
#[track_caller]
fn caught(f: impl FnOnce() -> Result<(), c_int>) -> c_int {
	let caller = std::panic::Location::caller();
	let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_or_else(
		|_| status::FAILED,
		|r| r.map_or_else(|c| c, |()| status::OK),
	);
	if code != status::OK && std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
		eprintln!("[ofx] image-effect suite error {code} at {caller}");
	}
	if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
		eprintln!("[ofx] image-effect suite call at {caller} -> {code}");
	}
	code
}

/// Borrows a C property-name string as `&str`.
///
/// A null `name` or non-UTF-8 content -> `ERR_VALUE`. The returned `&str` directly
/// borrows the caller's C string without copying; `'static` exists only so the borrow can span statements
/// inside the `caught` closure — its real lifetime is the whole callback.
///
/// # Safety
///
/// `name` must be null or point to a valid NUL-terminated C string, and that memory must stay
/// valid for the duration of the callback.
unsafe fn c_name<'a>(name: *const c_char) -> Result<&'a str, c_int> {
	if name.is_null() {
		return Err(status::ERR_VALUE);
	}
	unsafe { CStr::from_ptr(name) }
		.to_str()
		.map_err(|_| status::ERR_VALUE)
}

/// effect handle resolution (describe phase -> descriptor; instance phase -> instance).
enum EffectRef<'a> {
	Descriptor(&'a mut EffectDescriptor),
	Instance(&'a Instance),
}

/// Resolves an effect handle into a borrow of the descriptor or the instance.
///
/// Handles are built by `tag::make` with a kind tag; here the tag dispatches to
/// `EffectRef::Descriptor` (describe phase) or `EffectRef::Instance`
/// (instance phase). Null pointer or unknown tag -> `ERR_BAD_HANDLE`. The returned borrow is declared
/// `'static`: the handle's lifetime is guaranteed by the caller (for the duration of the C ABI callback), no
/// ownership check is done here.
fn resolve_effect(handle: *mut c_void) -> Result<EffectRef<'static>, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	// 两者 props 均在偏移 0（句柄约定）。
	unsafe {
		match tag::kind(handle) {
			tag::DESCRIPTOR => Ok(EffectRef::Descriptor(
				&mut *(tag::strip(handle) as *mut EffectDescriptor),
			)),
			tag::INSTANCE => Ok(EffectRef::Instance(
				&*(tag::strip(handle) as *const Instance),
			)),
			_ => Err(status::ERR_BAD_HANDLE),
		}
	}
}

/// clip handle resolution (instance phase; during describe the ClipDescriptor is used only in the property
/// suite and does not go through here).
fn resolve_clip(handle: *mut c_void) -> Result<&'static ClipInstance, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	unsafe {
		match tag::kind(handle) {
			tag::CLIP => Ok(&*(tag::strip(handle) as *const ClipInstance)),
			_ => Err(status::ERR_BAD_HANDLE),
		}
	}
}

/// getPropertySet: gets the effect's property-set handle.
///
/// The effect's property set is the handle itself (props at offset 0; mirrors HS:1886-1898
/// returning `getProps().getHandle()`, same address), so after validating the handle it is written back to `out` unchanged.
/// Both descriptors and instances are supported; a null `out` or a wrong handle kind -> `ERR_BAD_HANDLE`.
///
/// # Safety
///
/// `effect` must be a descriptor or instance handle that stays alive for the callback; `out` must be
/// writable.
unsafe extern "C" fn get_property_set(effect: *mut c_void, out: *mut *mut c_void) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let _ = resolve_effect(effect)?;
		unsafe { *out = effect };
		Ok(())
	})
}

/// getParamSet: gets the effect's param-set handle.
///
/// In this design the param set is the effect itself (in HS:1901-1931 the descriptor and the instance
/// each return their own param set; here the two are unified), so after validating the handle it is written back
/// to `out` unchanged. A null `out` or a wrong handle kind -> `ERR_BAD_HANDLE`.
///
/// # Safety
///
/// `effect` must be a descriptor or instance handle that stays alive for the callback; `out` must be
/// writable.
unsafe extern "C" fn get_param_set(effect: *mut c_void, out: *mut *mut c_void) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let _ = resolve_effect(effect)?;
		unsafe { *out = effect };
		Ok(())
	})
}

/// clipDefine (describe phase): defines a clip on the descriptor and returns its property-set handle.
///
/// `effect` must be a descriptor handle (an instance-phase call -> `ERR_BAD_HANDLE`); `name`
/// is the clip name, and redefining replaces the old clip wholesale (HS `defineClip`'s map-overwrite
/// semantics, ofxhImageEffect.cpp:265-271), so the old handle becomes invalid. On success `out`
/// receives the new clip's property-set handle (props at offset 0); on failure it is cleared first. `name`
/// being empty/non-UTF-8 -> `ERR_VALUE`; a null `out` -> `ERR_BAD_HANDLE`.
///
/// # Safety
///
/// `effect` must be a still-live descriptor handle; `name` must be a valid NUL-terminated
/// C string; `out` must be writable.
unsafe extern "C" fn clip_define(
	effect: *mut c_void,
	name: *const c_char,
	out: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		unsafe { *out = std::ptr::null_mut() };
		let n = unsafe { c_name(name)? };
		let desc = match resolve_effect(effect)? {
			EffectRef::Descriptor(d) => d,
			EffectRef::Instance(_) => return Err(status::ERR_BAD_HANDLE),
		};
		let clip = ClipDescriptor::new(n);
		// 重复定义：替换原 Box（HS map 覆盖语义）。必须是新的 Box：
		// 旧句柄随即失效（测试断言 clip != clip2），复用同一分配会
		// 破坏这一 C ABI 契约，故此处保留 `Box::new`。
		#[allow(clippy::replace_box)]
		if let Some(existing) = desc.clips.iter_mut().find(|c| c.name == n) {
			*existing = Box::new(clip);
		} else {
			desc.clips.push(Box::new(clip));
		}
		// 地址取自已入盒的对象（栈上临时变量在移动后失效）。
		let clip = desc
			.clips
			.iter()
			.find(|c| c.name == n)
			.expect("just stored");
		let addr = &clip.props as *const _ as usize;
		unsafe { *out = tag::make(addr as *const PropertySet, tag::CLIP) };
		Ok(())
	})
}

/// clipGetHandle (instance phase): gets a clip and its property set by name.
///
/// `name` is the name used at `clipDefine` time (a NUL-terminated C string); the two outputs `clip` and
/// `property_set` receive the same handle (the property set is at offset 0);
/// `property_set` may be null, meaning it is not needed. A descriptor handle, an unknown name, an empty name,
/// or non-UTF-8 -> `ERR_BAD_HANDLE` (HS:2067-2070).
///
/// # Safety
///
/// `effect` must be a still-live instance handle; `name` must be a valid NUL-terminated
/// C string; `clip` must be writable.
unsafe extern "C" fn clip_get_handle(
	effect: *mut c_void,
	name: *const c_char,
	clip: *mut *mut c_void,
	property_set: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if clip.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		unsafe { *clip = std::ptr::null_mut() };
		let n = unsafe { c_name(name)? };
		let handle = match resolve_effect(effect)? {
			EffectRef::Instance(i) => {
				let c = i
					.clips
					.iter()
					.find(|c| c.name == n)
					.ok_or(status::ERR_BAD_HANDLE)?;
				let addr = &c.props as *const _ as usize;
				tag::make(addr as *const PropertySet, tag::CLIP)
			}
			EffectRef::Descriptor(_) => return Err(status::ERR_BAD_HANDLE),
		};
		unsafe { *clip = handle };
		if !property_set.is_null() {
			// props 在偏移 0：clip handle 与 props handle 同值。
			unsafe { *property_set = handle };
		}
		Ok(())
	})
}

/// clipGetPropertySet: gets the clip's own property set.
///
/// The clip's property set is the clip handle itself (props at offset 0), so the tag is
/// validated first and then the handle is written back to `out` unchanged; a non-CLIP handle or null `out` -> `ERR_BAD_HANDLE`.
/// Consistent with the effect/param suite's "handle is the property set" convention.
///
/// # Safety
///
/// `clip` must be a still-live handle produced by the clip suite; `out` must be writable.
unsafe extern "C" fn clip_get_property_set(clip: *mut c_void, out: *mut *mut c_void) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let _ = resolve_clip(clip)?;
		unsafe { *out = clip };
		Ok(())
	})
}

/// clipGetImage: fetches the input image at `time`, registers it in the live table and returns the image property
/// set handle (HS:2003-2049).
///
/// `clip` is the clip handle; `time` is the frame time (the project timeline; seconds/frames are interpreted
/// by the render context); `region` is an optional pixel-coordinate rectangle, a null pointer meaning the whole frame. A clip named
/// `Output` returns the output image of the current render (set by the render driver via TLS);
/// with no output image -> `FAILED`; all other clips go through `fetch_image`, with the render scale taken from the current
/// render context (1:1 by default); a fetch failure -> `FAILED`. The returned handle must be released by
/// `clipReleaseImage`; the caller gets the property set, while the image itself is held by the live table.
///
/// `// TODO(clip)`: fetch_image awaits the clip migration to
/// the `quercus_core::texture::Texture` value model (frame access is currently a local stub).
///
/// # Safety
///
/// `clip` must be a still-live clip handle; `region` is null or points to a readable
/// `OfxRectD`; a null `out` -> `ERR_BAD_HANDLE`, otherwise it must be writable.
unsafe extern "C" fn clip_get_image(
	clip: *mut c_void,
	time: c_double,
	region: *const c_void,
	out: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		unsafe { *out = std::ptr::null_mut() };
		let c = resolve_clip(clip)?;
		// Output clip：返回当前渲染的输出图像（render 驱动经 TLS
		// 设置；对应 HS 渲染期输出图像挂在 Output clip 上）。
		if c.name == "Output" {
			if let Some(image) = crate::suites::current_output() {
				let addr = &image.props as *const _ as usize;
				LIVE_IMAGES
					.lock()
					.unwrap_or_else(|e| e.into_inner())
					.insert(addr, image);
				unsafe { *out = tag::make(addr as *const PropertySet, tag::IMAGE) };
				return Ok(());
			}
			return Err(status::FAILED);
		}
		// 区域（可空）：像素坐标的可选 bounds。
		let region = if region.is_null() {
			None
		} else {
			Some(unsafe { *(region as *const OfxRectD) })
		};
		// 渲染比例取自当前渲染上下文（HS 的 clip 在 render 期从
		// in_args 拿到 scale；本设计经 TLS，见 suites::RenderCtx）。
		let scale = crate::suites::render_ctx()
			.map(|ctx| ctx.scale)
			.unwrap_or(RenderScale { x: 1.0, y: 1.0 });
		let image = c
			.fetch_image(time, scale, region)
			.map_err(|_| status::FAILED)?;
		if std::env::var_os("QUERCUS_OFX_TRACE").is_some() {
			let p = image.pixels();
			let dump: Vec<f32> = p
				.chunks_exact(4)
				.take(4)
				.map(|c| f32::from_le_bytes(c.try_into().unwrap()))
				.collect();
			eprintln!("[ofx] clipGetImage({}) fetched pixels[0..4] = {dump:?}", c.name);
		}
		let image = std::sync::Arc::new(image);
		let addr = &image.props as *const _ as usize;
		LIVE_IMAGES
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.insert(addr, image);
		unsafe { *out = tag::make(addr as *const PropertySet, tag::IMAGE) };
		Ok(())
	})
}

/// clipReleaseImage: releases the image handle returned by `clipGetImage`.
///
/// Removes the strong reference from the live table (that table is the sole owner, so removal releases the image), matching
/// HS:2053-2068's releaseReference pairing. A null pointer, a non-IMAGE tag, and
/// releasing the same handle twice -> `ERR_BAD_HANDLE`.
///
/// # Safety
///
/// `image` must be a handle returned by `clipGetImage` and not yet released (or a null pointer);
/// after returning, the caller must not use that handle or the property set it points to.
unsafe extern "C" fn clip_release_image(image: *mut c_void) -> c_int {
	caught(|| {
		if image.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		if tag::kind(image) != tag::IMAGE {
			return Err(status::ERR_BAD_HANDLE);
		}
		let addr = tag::strip(image) as usize;
		let mut live = LIVE_IMAGES.lock().unwrap_or_else(|e| e.into_inner());
		match live.remove(&addr) {
			Some(_) => Ok(()),
			None => Err(status::ERR_BAD_HANDLE),
		}
	})
}

/// clipGetRegionOfDefinition: reads the clip's negotiated RoD (region of
/// definition, i.e. the clip's effective picture extent).
///
/// Reads four doubles from the clip instance property `OfxImageEffectPropRegionOfDefinition`
/// (HS:2111-2150); a missing or mistyped property, or an invalid rectangle with `x2 < x1 ||
/// y2 < y1`, -> `FAILED`. `_time` is unused: in phase 1 the RoD is independent of time
/// (the property is written by the host during negotiation). On success the rectangle (pixel coordinates,
/// `OfxRectD`) is written to `bounds`.
///
/// # Safety
///
/// `clip` must be a still-live handle produced by the clip suite; a null `bounds` ->
/// `ERR_BAD_HANDLE`, otherwise it must point to a writable `OfxRectD`.
unsafe extern "C" fn clip_get_region_of_definition(
	clip: *mut c_void,
	_time: c_double,
	bounds: *mut c_void,
) -> c_int {
	caught(|| {
		if bounds.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let c = resolve_clip(clip)?;
		let rod = read_rod(&c.props).ok_or(status::FAILED)?;
		if rod.x2 < rod.x1 || rod.y2 < rod.y1 {
			return Err(status::FAILED);
		}
		unsafe { *(bounds as *mut OfxRectD) = rod };
		Ok(())
	})
}

/// Reads the negotiated RoD from the clip properties (kOfxImageEffectPropRegionOfDefinition,
/// Double x4; written by the host during negotiation).
fn read_rod(props: &PropertySet) -> Option<OfxRectD> {
	let v = props.get("OfxImageEffectPropRegionOfDefinition", 0)?;
	let Value::Double(x1) = v else { return None };
	let Value::Double(y1) = props.get("OfxImageEffectPropRegionOfDefinition", 1)? else {
		return None;
	};
	let Value::Double(x2) = props.get("OfxImageEffectPropRegionOfDefinition", 2)? else {
		return None;
	};
	let Value::Double(y2) = props.get("OfxImageEffectPropRegionOfDefinition", 3)? else {
		return None;
	};
	Some(OfxRectD { x1, y1, x2, y2 })
}

/// abort: queries whether the current render should be cancelled (OFX's `abort` callback).
///
/// The contract (ofxImageEffect.h and ofxs's `abort(...) != 0`) is **0 = continue,
/// non-zero = abort** — it must not return REPLY_YES/NO (13 would be read by openfx-misc as
/// "abort immediately", and the pixel loop would write nothing -> black frame). The cancellation state is a successful answer rather than
/// an error code, so it does not go through `caught`'s Err channel; it only passes through the instance's cancel atomic
/// flag or `suites::progress`'s global cancellation: cancelled -> `1`, continue -> `0`.
/// A descriptor handle -> `ERR_BAD_HANDLE`; panic -> `FAILED`.
///
/// # Safety
///
/// `effect` must be an instance-phase handle (or a null pointer -> `ERR_BAD_HANDLE`) and must
/// stay alive for the callback — the plugin calls this repeatedly on the render thread.
unsafe extern "C" fn abort(effect: *mut c_void) -> c_int {
	catch_unwind(AssertUnwindSafe(|| {
		let inst = match resolve_effect(effect)? {
			EffectRef::Instance(i) => i,
			EffectRef::Descriptor(_) => return Err(status::ERR_BAD_HANDLE),
		};
		Ok(inst)
	}))
	.map_or_else(
		|_| status::FAILED,
		|r| match r {
			Ok(inst) => {
				let cancelled = inst.cancel.load(std::sync::atomic::Ordering::Relaxed)
					|| crate::suites::progress::is_cancelled();
				if cancelled { 1 } else { 0 }
			}
			Err(c) => c,
		},
	)
}

/// imageMemoryAlloc: allocates `byte_size` bytes for an image and writes them to `out`.
///
/// Uses the same ledger as the memory suite (HS:2174-2212), recording the pointer and length for
/// `image_memory_free` to settle. `_handle` (the effect handle) is ignored: the ledger is
/// global and does not account per "whoever allocates frees". A null `out` -> `ERR_VALUE`; an allocation
/// failure -> `ERR_MEMORY`, in which case `out` is not written.
///
/// # Safety
///
/// `out` must be null or point to a writable `*mut c_void`; `_handle` is unused and
/// may be any value.
unsafe extern "C" fn image_memory_alloc(
	_handle: *mut c_void,
	byte_size: c_int,
	out: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		match crate::suites::memory::alloc(byte_size as usize) {
			Some(ptr) => {
				unsafe { *out = ptr as *mut c_void };
				Ok(())
			}
			None => Err(status::ERR_MEMORY),
		}
	})
}

/// imageMemoryFree: returns memory allocated by `image_memory_alloc`.
///
/// Settles and frees it from the ledger (the ledger implementation is the same as the memory suite). An unknown or already freed
/// pointer -> `ERR_BAD_HANDLE` (SDK contract: double free is an error code, not undefined
/// behavior). Pairs with `image_memory_alloc`.
///
/// # Safety
///
/// `memory` must be null or a pointer returned by `image_memory_alloc` and not yet freed;
/// after returning, the caller must not read or write that memory.
unsafe extern "C" fn image_memory_free(memory: *mut c_void) -> c_int {
	caught(|| {
		if crate::suites::memory::free(memory as *mut u8) {
			Ok(())
		} else {
			Err(status::ERR_BAD_HANDLE)
		}
	})
}

/// imageMemoryLock: stub implementation of image-memory locking.
///
/// The HS (openfx host) semantics are "lock to prevent reallocation" (`ofxhMemory.cpp`'s
/// lock/unlock counting); the phase-1 ledger does not model lock counts, so this only writes the ledger's
/// address to `out` unchanged and returns success (see `memory.rs`), without changing the pointer.
/// Used in pairs with `image_memory_unlock`.
///
/// `out` receives the memory pointer; null -> `ERR_VALUE`. The address in the ledger is the data pointer
/// (allocation is immediately ready; there is no HS deferred allocation).
///
/// # Safety
///
/// `out` must be null or point to a writable `*mut c_void`; `memory` should be
/// a pointer returned by `image_memory_alloc`; this function neither validates nor dereferences it.
unsafe extern "C" fn image_memory_lock(memory: *mut c_void, out: *mut *mut c_void) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		// 账本里的地址即数据指针（分配即就绪，无 HS 的延迟分配）。
		unsafe { *out = memory };
		Ok(())
	})
}

/// imageMemoryUnlock: empty implementation of image-memory unlocking.
///
/// OFX image memory is two-stage "allocation + lock count"; the phase-1 ledger does not model lock counts
/// (see `memory.rs`), so unlocking only does panic fallback and always returns `status::OK`.
/// It must be called in pairs with `image_memory_lock`, but the call order does not affect the result.
///
/// # Safety
///
/// `_memory` is unused and may be any value (including a null pointer); this function does not dereference it
/// or validate that it still belongs to the ledger.
unsafe extern "C" fn image_memory_unlock(_memory: *mut c_void) -> c_int {
	caught(|| Ok(()))
}

use crate::property::Value;

/// Static function-table instance.
pub fn suite_v1() -> &'static ImageEffectSuiteV1 {
	static SUITE: std::sync::OnceLock<ImageEffectSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| ImageEffectSuiteV1 {
		get_property_set,
		get_param_set,
		clip_define,
		clip_get_handle,
		clip_get_property_set,
		clip_get_image,
		clip_release_image,
		clip_get_region_of_definition,
		abort,
		image_memory_alloc,
		image_memory_free,
		image_memory_lock,
		image_memory_unlock,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::CString;

	use crate::descriptor::EffectDescriptor;
	use crate::property::PropertySet;

	fn cs(s: &str) -> CString {
		CString::new(s).unwrap()
	}

	fn descriptor_handle(d: &EffectDescriptor) -> *mut c_void {
		tag::make(&d.props as *const PropertySet, tag::DESCRIPTOR)
	}

	/// describe phase: clipDefine creates a clip, the property suite reads and writes its properties,
	/// and redefinition replaces it wholesale (HS map overwrite).
	#[test]
	fn describe_clip_define_and_props() {
		let desc = EffectDescriptor::new();
		let s = suite_v1();
		let h = descriptor_handle(&desc);

		let mut clip: *mut c_void = std::ptr::null_mut();
		let name = cs("Source");
		unsafe {
			assert_eq!((s.clip_define)(h, name.as_ptr(), &mut clip), 0);
		}
		assert_eq!(tag::kind(clip), tag::CLIP);

		// 属性 suite 直接读写 clip handle（props 在偏移 0）。
		let ps = crate::suites::property::suite_v1();
		let label_prop = cs(crate::param::PROP_LABEL);
		let optional_prop = cs(crate::descriptor::CLIP_OPTIONAL);
		let label = cs("SourceLabel");
		unsafe {
			assert_eq!(
				(ps.set_string)(clip, label_prop.as_ptr(), 0, label.as_ptr()),
				0
			);
		}
		let mut out: *mut c_char = std::ptr::null_mut();
		unsafe {
			assert_eq!((ps.get_string)(clip, label_prop.as_ptr(), 0, &mut out), 0);
			assert_eq!(CStr::from_ptr(out).to_bytes(), b"SourceLabel");
			// 可选性标记：插件写 Optional=1。
			assert_eq!((ps.set_int)(clip, optional_prop.as_ptr(), 0, 1), 0);
		}

		// getPropertySet / getParamSet：返回 effect 本体 handle。
		let mut props: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.get_property_set)(h, &mut props), 0);
			assert_eq!(props, h);
			assert_eq!((s.get_param_set)(h, &mut props), 0);
			assert_eq!(props, h);
		}

		// 重复 clipDefine：替换（旧句柄失效，HS 一致）。
		let mut clip2: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.clip_define)(h, name.as_ptr(), &mut clip2), 0);
		}
		assert_ne!(clip, clip2);
		assert_eq!(desc.clips.len(), 1);

		// 空 out / 空 handle → BadHandle。
		unsafe {
			assert_eq!(
				(s.clip_define)(h, name.as_ptr(), std::ptr::null_mut()),
				status::ERR_BAD_HANDLE
			);
			assert_eq!(
				(s.clip_define)(std::ptr::null_mut(), name.as_ptr(), &mut clip2),
				status::ERR_BAD_HANDLE
			);
		}
	}

	/// abort: describe phase -> BadHandle; instance phase -> 0 (not cancelled).
	#[test]
	fn abort_requires_instance() {
		let desc = EffectDescriptor::new();
		let s = suite_v1();
		let dh = descriptor_handle(&desc);
		unsafe {
			assert_eq!((s.abort)(dh), status::ERR_BAD_HANDLE);
			assert_eq!((s.abort)(std::ptr::null_mut()), status::ERR_BAD_HANDLE);
		}
		// 实例期：未取消 → 0（OFX 契约：0 = 继续，非 0 = 中止）。
		let inst = std::sync::Arc::new(crate::instance::Instance {
			props: PropertySet::new(),
			plugin: std::sync::Arc::new(crate::host::Plugin {
				identifier: "t".into(),
				version: (1, 0),
				bundle_path: std::path::PathBuf::new(),
				contexts: vec![],
				descriptor: EffectDescriptor::new(),
				lib: std::ptr::null_mut(),
				entry: dummy_entry,
				ofx_plugin: std::ptr::null_mut(),
				unloaded: std::sync::atomic::AtomicBool::new(false),
			}),
			context: "OfxImageEffectContextFilter".into(),
			params: crate::param::ParamSetInstance { params: vec![] },
			clips: vec![],
			node_identity: std::sync::atomic::AtomicUsize::new(0),
			destroyed: std::sync::atomic::AtomicBool::new(false),
			sequence_range: std::sync::Mutex::new(None),
			progress_cb: std::sync::Mutex::new(None),
			cancel: std::sync::atomic::AtomicBool::new(false),
			edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
			render_lock: std::sync::Mutex::new(()),
			interact: std::sync::Mutex::new(None),
		});
		let ih = tag::make(&inst.props as *const PropertySet, tag::INSTANCE);
		unsafe {
			assert_eq!((s.abort)(ih), 0);
		}
	}

	unsafe extern "C" fn dummy_entry(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		status::OK
	}

	/// imageMemoryAlloc/Free go through the memory ledger.
	#[test]
	fn image_memory_ledger() {
		let s = suite_v1();
		let mut mem: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.image_memory_alloc)(std::ptr::null_mut(), 1024, &mut mem),
				0
			);
			assert!(!mem.is_null());
			// lock 返回数据指针（第 1 期 = 句柄本身）。
			let mut ptr: *mut c_void = std::ptr::null_mut();
			assert_eq!((s.image_memory_lock)(mem, &mut ptr), 0);
			assert_eq!(ptr, mem);
			assert_eq!((s.image_memory_unlock)(mem), 0);
			// 未知指针 free → BadHandle。
			assert_eq!(
				(s.image_memory_free)(0xdeadbeef as *mut c_void),
				status::ERR_BAD_HANDLE
			);
			assert_eq!((s.image_memory_free)(mem), 0);
		}
	}
}
