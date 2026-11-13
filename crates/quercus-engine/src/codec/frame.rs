//! C ABI for `quercus_codec::frame::Frame` (prefix `quercus_codec_frame_`): the
//! reference-counted CPU pixel buffer plugins read and write.
//!
//! The handle is Arc-backed (add_ref/release pair from
//! `export_handle!`). Mutation (`allocate`, `set_params`,
//! `set_timestamp`, writing through the `data` pointer) requires the
//! caller to hold the only live reference — the same discipline the C++
//! Frame has. The `data` pointer stays valid until the next `allocate`
//! or `set_params` on the frame, or its last release.

use quercus_codec::frame::Frame;

use crate::coretypes::{QuercusRational, QuercusVideoParams};
use crate::handle::{NativeMirror, into_ffi};

crate::export_handle!(Frame, quercus_codec_frame_add_ref, quercus_codec_frame_release);

/// An empty frame with default (invalid) params, ref count 1.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_codec_frame_new() -> *mut Frame {
	into_ffi(Frame::new()) as *mut Frame
}

/// A frame with a copy of `params` (line sizes computed), ref count 1.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_codec_frame_with_params(params: QuercusVideoParams) -> *mut Frame {
	into_ffi(Frame::with_params(params.to_native())) as *mut Frame
}

/// Allocates the pixel buffer per the current params.
///
/// Computes the line sizes and allocates width × height pixels at the frame's
/// format; on success the frame reports `is_allocated` and `data` becomes a
/// writable buffer. Returns `false` on a null handle, invalid params (an empty or
/// inconsistent geometry) or an allocation failure, and the frame then stays
/// unallocated.
///
/// # Safety
///
/// `this` must be null or a live frame handle, and the caller must hold the only
/// live reference: the old buffer is released and any pointer previously obtained
/// from `quercus_codec_frame_data` is invalidated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_codec_frame_allocate(this: *mut Frame) -> bool {
	if this.is_null() {
		return false;
	}
	unsafe { &mut *this }.allocate().is_ok()
}

/// Whether the frame's pixel buffer is allocated.
///
/// Returns `false` for a null handle and for a frame whose
/// `quercus_codec_frame_allocate` has not run (or failed) since creation or since
/// its params were replaced.
///
/// # Safety
///
/// `this` must be null or a live frame handle from this ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_codec_frame_is_allocated(this: *const Frame) -> bool {
	!this.is_null() && unsafe { &*this }.is_allocated()
}

/// The pixel buffer (writable); null until allocated.
///
/// Returns the start of the allocated pixel block, or null for a null handle or
/// an unallocated frame. The pointer stays valid until the next `allocate` /
/// `set_params` on the frame or until its last release (the module contract); it
/// is not NUL-terminated and its length is
/// `quercus_codec_frame_allocated_size`.
///
/// # Safety
///
/// `this` must be null or a live frame handle, and the caller must hold the only
/// live reference when writing through the returned pointer; the buffer must not
/// be used after the validity window above.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_codec_frame_data(this: *mut Frame) -> *mut u8 {
	if this.is_null() {
		return std::ptr::null_mut();
	}
	match unsafe { &mut *this }.data_mut() {
		Some(data) => data.as_mut_ptr(),
		None => std::ptr::null_mut(),
	}
}

/// The allocated buffer size in bytes (0 when unallocated).
///
/// This is the size the current params required at the last successful
/// `quercus_codec_frame_allocate`, not the size the params would need now (a
/// later `set_params` does not reallocate). Returns `0` for a null handle too.
///
/// # Safety
///
/// `this` must be null or a live frame handle from this ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_codec_frame_allocated_size(this: *const Frame) -> usize {
	if this.is_null() {
		return 0;
	}
	unsafe { &*this }.allocated_size()
}

macro_rules! frame_getter {
	($( $name:ident => $native:ident ),* $(,)?) => {
		$(
			#[doc = concat!("`Frame::", stringify!($native), "` (0 on null).")]
			#[unsafe(no_mangle)]
			pub unsafe extern "C" fn $name(this: *const Frame) -> i32 {
				if this.is_null() {
					return 0;
				}
				unsafe { &*this }.$native()
			}
		)*
	};
}

frame_getter! {
	quercus_codec_frame_linesize_bytes => linesize_bytes,
	quercus_codec_frame_linesize_pixels => linesize_pixels,
	quercus_codec_frame_width => width,
	quercus_codec_frame_height => height,
	quercus_codec_frame_channel_count => channel_count,
}

/// The frame's pixel format as a `PixelFormat` code.
///
/// The code mirrors `quercus_core::PixelFormat` (0 = U8, 1 = U10, 2 = U16,
/// 3 = F16, 4 = F32 — F32 is the pipeline default). Returns `0` for a null
/// handle, which is indistinguishable from U8, so check the handle first when
/// that matters.
///
/// # Safety
///
/// `this` must be null or a live frame handle from this ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_codec_frame_format(this: *const Frame) -> i32 {
	if this.is_null() {
		return 0;
	}
	unsafe { &*this }.format() as i32
}

/// The frame's timestamp as a rational in seconds.
///
/// Returns the stored `num / den` (not normalized). A null handle returns
/// `{num: 0, den: 0}`, so check the handle before trusting the value. Pairs with
/// `quercus_codec_frame_set_timestamp`.
///
/// # Safety
///
/// `this` must be null or a live frame handle from this ABI. The handle is
/// borrowed, not released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_codec_frame_timestamp(this: *const Frame) -> QuercusRational {
	if this.is_null() {
		return QuercusRational { num: 0, den: 0 };
	}
	QuercusRational::from_native(&unsafe { &*this }.timestamp())
}

/// Sets the frame's timestamp.
///
/// `ts` is a rational in seconds (numerator / denominator) copied into the
/// frame; pixel data and params are untouched. A null `this` is a no-op. Pairs
/// with `quercus_codec_frame_timestamp`.
///
/// # Safety
///
/// `this` must be null or a live frame handle, and the caller must hold the only
/// live reference while mutating it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_codec_frame_set_timestamp(this: *mut Frame, ts: QuercusRational) {
	if this.is_null() {
		return;
	}
	unsafe { &mut *this }.set_timestamp(ts.to_native());
}

/// Copies the frame's params into `out`.
///
/// `this` is the frame and `out` the caller-owned struct written on success; the
/// copy carries the line sizes computed for the current params. Returns `false`
/// on a null `this` or `out`, and on a default-constructed frame without params;
/// `out` is left untouched in those cases.
///
/// # Safety
///
/// `this` must be null or a live frame handle; `out` must be null or point to a
/// writable `QuercusVideoParams`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_codec_frame_get_params(
	this: *const Frame,
	out: *mut QuercusVideoParams,
) -> bool {
	if this.is_null() || out.is_null() {
		return false;
	}
	match unsafe { &*this }.params() {
		Some(params) => {
			unsafe { *out = QuercusVideoParams::from_native(params) };
			true
		}
		None => false,
	}
}

/// Replaces the frame's params (recomputes line sizes, does NOT reallocate the
/// buffer).
///
/// `params` is copied in by value; line sizes follow the new width / height /
/// format, but the existing pixel allocation (and its size) is untouched, so call
/// `quercus_codec_frame_allocate` afterwards when the geometry grew. A null
/// `this` is a no-op.
///
/// # Safety
///
/// `this` must be null or a live frame handle, and the caller must hold the only
/// live reference (the same discipline the C++ `Frame` has): mutating a frame
/// another owner is reading is not synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_codec_frame_set_params(this: *mut Frame, params: QuercusVideoParams) {
	if this.is_null() {
		return;
	}
	unsafe { &mut *this }.set_params(params.to_native());
}

#[cfg(test)]
mod tests {
	use super::*;

	fn test_params(width: i32, height: i32) -> QuercusVideoParams {
		let mut p = crate::coretypes::quercus_core_videoparams_default();
		p.width = width;
		p.height = height;
		p.format = 4; // F32
		p.channel_count = 4;
		p
	}

	#[test]
	fn frame_lifecycle() {
		unsafe {
			let frame = quercus_codec_frame_with_params(test_params(64, 32));
			assert!(!frame.is_null());
			assert!(!quercus_codec_frame_is_allocated(frame));
			assert!(quercus_codec_frame_data(frame).is_null());

			assert!(quercus_codec_frame_allocate(frame));
			assert!(quercus_codec_frame_is_allocated(frame));
			assert_eq!(quercus_codec_frame_width(frame), 64);
			assert_eq!(quercus_codec_frame_height(frame), 32);
			assert_eq!(quercus_codec_frame_format(frame), 4);
			assert_eq!(quercus_codec_frame_channel_count(frame), 4);

			let size = quercus_codec_frame_allocated_size(frame);
			assert_eq!(size, 64 * 32 * 4 * 4, "w*h*channels*f32");
			assert_eq!(quercus_codec_frame_linesize_bytes(frame), 64 * 4 * 4);

			// The buffer is writable through the data pointer.
			let data = quercus_codec_frame_data(frame);
			assert!(!data.is_null());
			std::ptr::write_bytes(data, 0xAB, 16);

			// Timestamp roundtrip.
			quercus_codec_frame_set_timestamp(frame, QuercusRational { num: 3, den: 2 });
			assert_eq!(
				quercus_codec_frame_timestamp(frame),
				QuercusRational { num: 3, den: 2 }
			);

			// Params roundtrip.
			let mut out = quercus_codec_frame_new();
			let mut p = crate::coretypes::quercus_core_videoparams_default();
			assert!(quercus_codec_frame_get_params(frame, &mut p));
			assert_eq!((p.width, p.height), (64, 32));
			quercus_codec_frame_release(out.cast());

			quercus_codec_frame_release(frame);
		}
	}

	#[test]
	fn frame_refcount_keeps_buffer_alive() {
		unsafe {
			let frame = quercus_codec_frame_with_params(test_params(8, 8));
			assert!(quercus_codec_frame_allocate(frame));
			quercus_codec_frame_add_ref(frame);
			quercus_codec_frame_release(frame);
			assert!(quercus_codec_frame_is_allocated(frame), "one reference remains");
			quercus_codec_frame_release(frame);
		}
	}

	#[test]
	fn frame_null_is_safe() {
		unsafe {
			assert!(!quercus_codec_frame_allocate(std::ptr::null_mut()));
			assert!(!quercus_codec_frame_is_allocated(std::ptr::null()));
			assert!(quercus_codec_frame_data(std::ptr::null_mut()).is_null());
			assert_eq!(quercus_codec_frame_allocated_size(std::ptr::null()), 0);
			assert_eq!(quercus_codec_frame_width(std::ptr::null()), 0);
			assert_eq!(
				quercus_codec_frame_timestamp(std::ptr::null()),
				QuercusRational { num: 0, den: 0 }
			);
			quercus_codec_frame_set_timestamp(std::ptr::null_mut(), QuercusRational { num: 1, den: 1 });
			assert!(!quercus_codec_frame_get_params(std::ptr::null(), std::ptr::null_mut()));
			quercus_codec_frame_set_params(std::ptr::null_mut(), test_params(1, 1));
			quercus_codec_frame_add_ref(std::ptr::null());
			quercus_codec_frame_release(std::ptr::null());
		}
	}
}
