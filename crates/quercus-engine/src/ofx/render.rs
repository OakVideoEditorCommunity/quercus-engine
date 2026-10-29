//! Headless frame rendering (`quercus_ofx_instance_render_*`): the CPU
//! path of `render_driver::render_frame` with plain RGBA F32 buffers
//! — the "apply an effect to a frame" primitive functional plugins
//! (batch processing, AI-assisted editing) build on.
//!
//! Set parameters first (`quercus_ofx_param_set_*`); long renders report
//! progress through the registered handler
//! ([`crate::ofx::host::quercus_ofx_set_progress_handler`]) and honor the
//! instance's cancel flag.

use quercus_ofx_plugin::instance::{OfxRangeD, OfxRectD, RenderScale};
use quercus_ofx_plugin::node_factory;
use quercus_ofx_plugin::render::{self, PIXEL_FORMAT_F32, VideoParams};
use quercus_ofx_plugin::render_driver::{self, RenderJob};

use crate::handle::NativeMirror;

/// A rectangle in canonical coordinates (`repr(C)` mirror of
/// `quercus_ofx_plugin::instance::OfxRectD` — see
/// [`NativeMirror`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QuercusOfxRectD {
	pub x1: f64,
	pub y1: f64,
	pub x2: f64,
	pub y2: f64,
}

impl NativeMirror for QuercusOfxRectD {
	type Native = OfxRectD;

	fn from_native(r: &OfxRectD) -> Self {
		Self {
			x1: r.x1,
			y1: r.y1,
			x2: r.x2,
			y2: r.y2,
		}
	}

	fn to_native(&self) -> OfxRectD {
		OfxRectD {
			x1: self.x1,
			y1: self.y1,
			x2: self.x2,
			y2: self.y2,
		}
	}
}

fn f32_texture(width: i32, height: i32, pixels: &[u8]) -> Option<render::Texture> {
	let mut params = VideoParams::default();
	params.width = width;
	params.height = height;
	params.format = PIXEL_FORMAT_F32;
	render::texture_create(&params, pixels, 0).ok()
}

/// Renders one frame through the instance's effect (CPU path):
/// `src` is the input frame (tight RGBA F32, `src_w`×`src_h`; null
/// for sources/generators), the output is written to `dst` (tight
/// RGBA F32, `dst_w`×`dst_h`; the buffer must hold
/// `dst_w*dst_h*4` floats). False on any failure: unknown instance,
/// invalid buffers, a cancelled render, or a plugin error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_instance_render_frame(
	id: u64,
	time: f64,
	src: *const f32,
	src_w: i32,
	src_h: i32,
	dst: *mut f32,
	dst_w: i32,
	dst_h: i32,
) -> bool {
	let Some(inst) = node_factory::instance_from_id(id) else {
		return false;
	};
	if dst.is_null() || dst_w <= 0 || dst_h <= 0 {
		return false;
	}
	let (dst_wu, dst_hu) = (dst_w as usize, dst_h as usize);

	let src_tex = if src.is_null() {
		None
	} else {
		if src_w <= 0 || src_h <= 0 {
			return false;
		}
		let bytes = unsafe {
			std::slice::from_raw_parts(src as *const u8, src_w as usize * src_h as usize * 4 * 4)
		};
		match f32_texture(src_w, src_h, bytes) {
			Some(tex) => Some(tex),
			None => return false,
		}
	};

	let dst_tex = match f32_texture(dst_w, dst_h, &vec![0u8; dst_wu * dst_hu * 4 * 4]) {
		Some(tex) => tex,
		None => return false,
	};

	let job = RenderJob {
		time,
		dst: dst_tex,
		src: src_tex,
		..Default::default()
	};
	let Ok((out, _rois)) = render_driver::render_frame(&inst.value, &job) else {
		return false;
	};
	let Ok(frame) = render::texture_get_frame(&out) else {
		return false;
	};
	let params = frame.video_params();
	if params.width != dst_w || params.height != dst_h || params.format != PIXEL_FORMAT_F32 {
		return false;
	}
	// Copy row by row (the frame's stride may be padded; the output is
	// tight RGBA F32).
	let tight = dst_wu * 4 * 4;
	let row = frame.linesize_bytes().max(tight);
	if frame.data.len() < row * (dst_hu - 1) + tight {
		return false;
	}
	let out_slice = unsafe { std::slice::from_raw_parts_mut(dst as *mut u8, tight * dst_hu) };
	for y in 0..dst_hu {
		out_slice[y * tight..(y + 1) * tight]
			.copy_from_slice(&frame.data[y * row..y * row + tight]);
	}
	true
}

/// The beginSequenceRender bracket (CPU mode): opens a sequence
/// render over `[min, max]` (seconds). Pair with
/// [`quercus_ofx_instance_end_sequence`]. False on unknown instance or a
/// plugin error.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_instance_begin_sequence(id: u64, min: f64, max: f64) -> bool {
	let Some(inst) = node_factory::instance_from_id(id) else {
		return false;
	};
	render_driver::begin_sequence(&inst.value, OfxRangeD { min, max }, None).is_ok()
}

/// The endSequenceRender bracket (CPU mode), pairing
/// [`quercus_ofx_instance_begin_sequence`].
#[unsafe(no_mangle)]
pub extern "C" fn quercus_ofx_instance_end_sequence(id: u64, min: f64, max: f64) -> bool {
	let Some(inst) = node_factory::instance_from_id(id) else {
		return false;
	};
	render_driver::end_sequence(&inst.value, OfxRangeD { min, max }, None).is_ok()
}

/// The instance's region of definition at `time` (canonical
/// coordinates; render scale 1:1). False on unknown instance or a
/// plugin error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn quercus_ofx_instance_get_rod(
	id: u64,
	time: f64,
	out: *mut QuercusOfxRectD,
) -> bool {
	if out.is_null() {
		return false;
	}
	let Some(inst) = node_factory::instance_from_id(id) else {
		return false;
	};
	let Ok(rod) = inst
		.value
		.get_region_of_definition(time, RenderScale { x: 1.0, y: 1.0 })
	else {
		return false;
	};
	unsafe { *out = QuercusOfxRectD::from_native(&rod) };
	true
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn unknown_instances_and_bad_buffers_fail_closed() {
		let mut dst = [0.0f32; 4];
		assert!(!unsafe {
			quercus_ofx_instance_render_frame(
				u64::MAX,
				0.0,
				std::ptr::null(),
				0,
				0,
				dst.as_mut_ptr(),
				1,
				1,
			)
		});
		// Even a (hypothetical) valid id rejects a null output.
		assert!(!unsafe {
			quercus_ofx_instance_render_frame(
				0,
				0.0,
				std::ptr::null(),
				0,
				0,
				std::ptr::null_mut(),
				1,
				1,
			)
		});
		assert!(!quercus_ofx_instance_begin_sequence(u64::MAX, 0.0, 1.0));
		assert!(!quercus_ofx_instance_end_sequence(u64::MAX, 0.0, 1.0));
		let mut rod = QuercusOfxRectD::default();
		assert!(!unsafe { quercus_ofx_instance_get_rod(u64::MAX, 0.0, &mut rod) });
		assert!(!unsafe { quercus_ofx_instance_get_rod(0, 0.0, std::ptr::null_mut()) });
	}
}
