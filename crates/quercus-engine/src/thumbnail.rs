//! Immutable owned thumbnail frames exported by asynchronous render tasks.

use crate::handle::NativeMirror;

/// CPU thumbnail pixels in tight F32 RGBA order.
#[derive(Clone, Debug)]
pub struct ThumbnailData {
	pub width: i32,
	pub height: i32,
	pub pixels: Vec<f32>,
}

/// Thumbnail layout without the variable-length pixel buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThumbnailMetadata {
	/// Image width in pixels.
	pub width: i32,
	/// Image height in pixels.
	pub height: i32,
	/// Number of F32 values in the packed RGBA buffer.
	pub pixel_count: usize,
}

impl ThumbnailData {
	fn metadata(&self) -> ThumbnailMetadata {
		ThumbnailMetadata {
			width: self.width,
			height: self.height,
			pixel_count: self.pixels.len(),
		}
	}
}

/// Opaque reference-counted thumbnail result.
pub struct ThumbnailHandle {
	pub(crate) data: ThumbnailData,
}

crate::export_handle!(
	ThumbnailHandle,
	quercus_thumbnail_add_ref,
	quercus_thumbnail_release
);

impl ThumbnailHandle {
	/// Wraps owned thumbnail data in a fresh ref-counted FFI handle and
	/// returns its raw pointer.
	///
	/// The handle starts with one reference; the caller (the task result
	/// slot) transfers that reference to whoever takes the thumbnail and must
	/// eventually release it with `quercus_thumbnail_release`.
	pub(crate) fn from_data(data: ThumbnailData) -> *const Self {
		crate::handle::into_ffi(Self { data })
	}
}

/// Width, height and number of floats in the packed RGBA pixel buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct QuercusThumbnailInfo {
	/// Image width in pixels.
	pub width: i32,
	/// Image height in pixels.
	pub height: i32,
	/// Number of F32 values in the packed RGBA buffer.
	pub pixel_count: usize,
}

impl NativeMirror for QuercusThumbnailInfo {
	type Native = ThumbnailMetadata;

	fn from_native(native: &Self::Native) -> Self {
		Self {
			width: native.width,
			height: native.height,
			pixel_count: native.pixel_count,
		}
	}

	fn to_native(&self) -> Self::Native {
		Self::Native {
			width: self.width,
			height: self.height,
			pixel_count: self.pixel_count,
		}
	}
}

/// Reads a thumbnail's width, height and packed-pixel float count into
/// `out`.
///
/// Returns `false` when `thumbnail` or `out` is null (nothing is
/// written), and `true` after a successful copy. The metadata describes
/// the immutable pixel buffer, so it cannot change after creation.
///
/// # Safety
///
/// `thumbnail` must be null or a live handle returned by
/// `quercus_task_take_thumbnail` and not yet released; `out` must be null
/// or point to writable storage for one [`QuercusThumbnailInfo`] valid
/// for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_thumbnail_info(
	thumbnail: *const ThumbnailHandle,
	out: *mut QuercusThumbnailInfo,
) -> bool {
	let (Some(thumbnail), Some(out)) = (unsafe { thumbnail.as_ref() }, unsafe { out.as_mut() })
	else {
		return false;
	};
	*out = QuercusThumbnailInfo::from_native(&thumbnail.data.metadata());
	true
}

/// Copies a thumbnail's tightly packed F32 RGBA pixels into `dst`.
///
/// `dst_capacity` is the number of `f32` values the caller allocated
/// (`width * height * 4` for the full frame). On success exactly
/// `pixel_count` floats are copied and that count is returned.
///
/// Returns `0` and writes nothing when `thumbnail` or `dst` is null, or
/// when `dst_capacity` is smaller than `pixel_count`; call
/// [`quercus_thumbnail_info`] first to size the buffer. The thumbnail is
/// immutable, so the required count is stable between the two calls.
///
/// # Safety
///
/// `thumbnail` must be null or a live handle from
/// `quercus_task_take_thumbnail` that has not been released. `dst` must
/// be null or point to `dst_capacity` writable `f32` values, and the
/// source and destination regions must not overlap (the copy uses
/// `copy_nonoverlapping`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_thumbnail_copy_pixels(
	thumbnail: *const ThumbnailHandle,
	dst: *mut f32,
	dst_capacity: usize,
) -> usize {
	let Some(thumbnail) = (unsafe { thumbnail.as_ref() }) else {
		return 0;
	};
	if dst.is_null() || dst_capacity < thumbnail.data.pixels.len() {
		return 0;
	}
	unsafe {
		std::ptr::copy_nonoverlapping(
			thumbnail.data.pixels.as_ptr(),
			dst,
			thumbnail.data.pixels.len(),
		)
	};
	thumbnail.data.pixels.len()
}
