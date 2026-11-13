// ---------------------------------------------------------------------------
// GPU device adoption (zero-copy present / shared device)
// ---------------------------------------------------------------------------

use quercus_core::backend::{BackendKind, GpuContext};
use quercus_core::wgpu;
use std::ffi::c_void;
use std::sync::Arc;

/// A [`BackendKind`] from its C ABI discriminant (0..=4, the enum order).
fn backend_kind_from_abi(kind: i32) -> Option<BackendKind> {
	match kind {
		0 => Some(BackendKind::Auto),
		1 => Some(BackendKind::Metal),
		2 => Some(BackendKind::Vulkan),
		3 => Some(BackendKind::Gl),
		4 => Some(BackendKind::Cpu),
		_ => None,
	}
}

/// Clone a host `wgpu::Device`/`wgpu::Queue` pair into an adopted context.
///
/// # Safety
///
/// `device` and `queue` must be non-null and point to live, aligned
/// `wgpu::Device` / `wgpu::Queue` values from the same wgpu 29 generation
/// the engine was built with. They are borrowed for the duration of the
/// call only; the context clones the underlying handles, so the host
/// keeps ownership.
unsafe fn adopt_raw(
	device: *const c_void,
	queue: *const c_void,
	kind: BackendKind,
) -> Arc<GpuContext> {
	// SAFETY: the caller guarantees both pointers are live wgpu values.
	let device = unsafe { &*(device as *const wgpu::Device) };
	let queue = unsafe { &*(queue as *const wgpu::Queue) };
	GpuContext::adopt(Arc::new(device.clone()), Arc::new(queue.clone()), kind)
}

/// `quercus_render_gpu_install`: adopt the host's wgpu device/queue so
/// the engine renders on the presenter's device (zero-copy present).
///
/// The pointers must reference live `wgpu::Device` / `wgpu::Queue` values
/// from the **same wgpu major version (29)** the engine was built with —
/// the C ABI cannot verify that, and mixing generations is undefined
/// behavior. The engine clones the handles and takes no ownership: the
/// host keeps ownership and must keep the objects alive only for the
/// duration of this call.
///
/// **Timing guard**: call this before the first render/thumbnail.
/// [`GpuContext::install_shared`] refuses once the process-wide context
/// has created GPU resources (`false`; the engine falls back to the
/// single staging readback) or when an adopted context is already
/// installed. `backend_kind` is a [`BackendKind`] discriminant (0 auto,
/// 1 Metal, 2 Vulkan, 3 GL, 4 CPU); the live device does not expose its
/// backend. False on a null pointer, an out-of-range kind, or a refused
/// install.
///
/// # Safety
///
/// Both pointers must be non-null and point to live, aligned
/// `wgpu::Device` / `wgpu::Queue` values of the wgpu 29 generation.
/// Installs a display LUT on the host-adopted context.
///
/// # Safety
/// `lo` and `hi` point to three floats and `data` points to `len` floats.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_render_gpu_install(
	wgpu_device: *const c_void,
	wgpu_queue: *const c_void,
	backend_kind: i32,
) -> bool {
	if wgpu_device.is_null() || wgpu_queue.is_null() {
		return false;
	}
	let Some(kind) = backend_kind_from_abi(backend_kind) else {
		return false;
	};
	// A host device is untrusted and foreign; a panic must not unwind
	// across the C ABI boundary.
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		// SAFETY: checked non-null; the caller upholds the pointer contract.
		let ctx = unsafe { adopt_raw(wgpu_device, wgpu_queue, kind) };
		GpuContext::install_shared(Some(ctx))
	}))
	.unwrap_or(false)
}

/// `quercus_render_gpu_init`: eagerly create the engine-owned shared GPU
/// context with an explicit backend (0 auto, 1 Metal, 2 Vulkan, 3 GL,
/// 4 CPU). Unlike [`quercus_render_gpu_install`] this needs no host wgpu
/// pointers, so pure C hosts can initialize GPU rendering; the engine
/// creates and owns the device exactly like the lazy first-render path.
///
/// Only the first initialization chooses the backend: once the shared
/// context exists (an earlier init, render, or adopt) later calls are
/// no-ops. Returns whether a GPU context is available afterwards; `false`
/// means rendering stays on the CPU path (requesting CPU therefore always
/// returns `false`). Does not set the host-installed flag — call
/// [`quercus_render_gpu_mark_host`] for the hardware-surface import gate.
/// A driver panic during device creation is contained and reported as
/// `false` instead of unwinding across the C ABI.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_render_gpu_init(backend_kind: i32) -> bool {
	let Some(kind) = backend_kind_from_abi(backend_kind) else {
		return false;
	};
	// Device creation runs driver code; a panic must not unwind across
	// the C ABI boundary.
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		GpuContext::init_shared(kind)
	}))
	.unwrap_or(false)
}

/// The shared context's backend as a [`BackendKind`] discriminant (0..=4),
/// or -1 when no context exists.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_render_gpu_kind() -> i32 {
	GpuContext::shared()
		.map(|ctx| ctx.kind() as i32)
		.unwrap_or(-1)
}

/// True when the shared context wraps a host-adopted device (zero-copy
/// present); false when none / engine-created.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_render_gpu_is_adopted() -> bool {
	GpuContext::shared().is_some_and(|ctx| ctx.is_adopted())
}

/// True when the host installed or marked the shared device (the M5
/// hardware-surface import gate).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_render_gpu_host_installed() -> bool {
	GpuContext::host_gpu_installed()
}

/// Declares the process's shared context as the HOST's render device,
/// creating it on first use when the engine has not yet (macOS/Windows,
/// where the windowing layer does not hand out its device). False when
/// no adapter is available.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_render_gpu_mark_host() -> bool {
	GpuContext::mark_host_context()
}

/// Installs the CPU-baked working-space → display-encoded 3D LUT on the
/// host-adopted GPU context (the zero-copy present path).
///
/// `edge` is the cube edge (samples per axis), so `len` must be exactly
/// `edge³ * 3`; `lo` and `hi` are the 3 working-space RGB values at the cube's
/// first and last corner, and `data` is the flattened RGB table in the
/// engine's red-major order. The table is uploaded as an `edge³` 3D texture
/// and replaces any previous display LUT. Returns `false` when a pointer is
/// null, `len` does not match `edge` (or `edge³ * 3` overflows), no
/// host-adopted context exists — the CPU display transform then stays
/// authoritative — or the GPU upload fails.
///
/// The safe wrapper is `quercus_engine_rs::render::gpu_set_display_lut` (the
/// app calls it right after installing the shared device).
///
/// # Safety
///
/// `lo` and `hi` must be null or point to 3 readable `f32`s, and `data` must
/// be null or point to `len` readable `f32`s. Null pointers are rejected
/// with `false` before any dereference; non-null pointers are dereferenced
/// for the duration of the call only (the table is copied).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_render_gpu_set_display_lut(
	edge: u32,
	lo: *const f32,
	hi: *const f32,
	data: *const f32,
	len: usize,
) -> bool {
	if lo.is_null() || hi.is_null() || data.is_null() {
		return false;
	}
	let expected = (edge as usize).checked_pow(3).and_then(|n| n.checked_mul(3));
	if expected != Some(len) {
		return false;
	}
	let lut = quercus_core::lut::Lut3d {
		edge,
		lo: unsafe { std::slice::from_raw_parts(lo, 3) }.try_into().unwrap(),
		hi: unsafe { std::slice::from_raw_parts(hi, 3) }.try_into().unwrap(),
		data: unsafe { std::slice::from_raw_parts(data, len) }.to_vec(),
	};
	GpuContext::shared()
		.filter(|ctx| ctx.is_adopted())
		.is_some_and(|ctx| ctx.set_display_lut(&lut).is_ok())
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Argument validation happens before any pointer dereference or
	/// device work, so this test needs no GPU.
	#[test]
	fn render_gpu_install_rejects_null_and_out_of_range_kind() {
		unsafe {
			assert!(!quercus_render_gpu_install(
				std::ptr::null(),
				std::ptr::null(),
				0
			));
			assert!(!quercus_render_gpu_install(
				std::ptr::null(),
				std::ptr::null(),
				4
			));
			assert!(!quercus_render_gpu_install(
				std::ptr::null(),
				std::ptr::null(),
				-1
			));
			assert!(!quercus_render_gpu_install(
				std::ptr::null(),
				std::ptr::null(),
				99
			));
		}
		assert_eq!(backend_kind_from_abi(-1), None);
		assert_eq!(backend_kind_from_abi(5), None);
		// The ABI discriminants are exactly the BackendKind order.
		for (value, kind) in [
			(0, BackendKind::Auto),
			(1, BackendKind::Metal),
			(2, BackendKind::Vulkan),
			(3, BackendKind::Gl),
			(4, BackendKind::Cpu),
		] {
			assert_eq!(backend_kind_from_abi(value), Some(kind));
			assert_eq!(kind as i32, value, "ABI discriminants follow BackendKind");
		}
	}

	/// An out-of-range kind is rejected before the process-wide slot is
	/// touched, so this needs no GPU and cannot disturb other tests.
	#[test]
	fn render_gpu_init_rejects_out_of_range_kind() {
		assert!(!quercus_render_gpu_init(-1));
		assert!(!quercus_render_gpu_init(5));
		assert!(!quercus_render_gpu_init(i32::MIN));
	}

	/// Explicit initialization creates (or finds) the engine-owned context:
	/// repeated calls are no-ops with the same answer, and the answer agrees
	/// with the backend query. No adapter is a soft outcome (`false`), not a
	/// failure, matching the lazy `shared()` path the queries already
	/// exercise.
	#[test]
	fn render_gpu_init_is_idempotent_and_consistent_with_kind() {
		let available = quercus_render_gpu_init(0);
		assert_eq!(
			available,
			quercus_render_gpu_init(0),
			"init is a no-op once decided"
		);
		assert_eq!(available, quercus_render_gpu_kind() >= 0);
	}

	/// The query helpers read the process-wide slot and must never panic
	/// without an installed context. They may lazily create the engine
	/// context (`shared()` semantics), so only the answer's shape is
	/// asserted. `mark_host` is deliberately not exercised: it declares
	/// (and may create) a process-wide device and would race the other
	/// render tests.
	#[test]
	fn render_gpu_queries_never_panic_without_an_installed_context() {
		let kind = quercus_render_gpu_kind();
		assert!(kind == -1 || (0..=4).contains(&kind));
		assert!(!quercus_render_gpu_host_installed() || kind >= 0);
		let _ = quercus_render_gpu_is_adopted();
	}

	/// The pointer path clones the host handles into an independent
	/// adopted context. This calls the private adopt helper directly (not
	/// the C export) so it never touches the process-wide shared slot and
	/// cannot race the other render tests; an install-into-slot test is
	/// skipped because the slot is process-global. No adapter is a soft
	/// skip (hard fail under `QUERCUS_REQUIRE_GPU`).
	#[test]
	fn render_gpu_adopt_clones_a_live_host_device() {
		let Some(base) = quercus_core::backend::gpu_or_skip("gpu device adoption") else {
			return;
		};
		let (device, queue) = base.device_queue();
		let kind = base.kind();
		// SAFETY: both pointers reference live values for the call.
		let adopted = unsafe {
			adopt_raw(
				Arc::as_ptr(&device) as *const c_void,
				Arc::as_ptr(&queue) as *const c_void,
				kind,
			)
		};
		assert!(
			adopted.is_adopted(),
			"an adopted context wraps a host device"
		);
		assert_eq!(adopted.kind(), kind);
		assert!(!adopted.is_used(), "no GPU resources were created yet");
		// The host keeps ownership; dropping its handles leaves the
		// engine's clones alive.
		drop(base);
		drop(device);
		drop(queue);
		assert!(adopted.create_texture(2, 2).is_ok());
	}
}
