use super::*;

unsafe extern "C" {
	#[link_name = "quercus_vec_string_len"]
	fn vec_len(this: *const QuercusVecString) -> usize;
	#[link_name = "quercus_vec_string_get"]
	fn vec_get(this: *const QuercusVecString, index: usize, out_len: *mut usize) -> *const u8;
	#[link_name = "quercus_vec_string_release"]
	unsafe fn vec_release(this: *const QuercusVecString);
}

#[test]
fn decoder_ids_include_ffmpeg() {
	unsafe {
		let ids = quercus_codec_decoder_ids();
		let mut found = false;
		for i in 0..vec_len(ids) {
			let mut len = 0usize;
			let ptr = vec_get(ids, i, &mut len);
			let id = std::str::from_utf8(std::slice::from_raw_parts(ptr, len)).unwrap();
			found |= id == "ffmpeg";
		}
		vec_release(ids);
		assert!(found, "the FFmpeg decoder must be registered");
	}
}

#[test]
fn image_sequence_helpers() {
	unsafe {
		// Trailing-digit stems mark a sequence: "frame_0001.png".
		let name = "frame_0001.png";
		let digits = quercus_codec_decoder_image_sequence_digit_count(name.as_ptr(), name.len());
		assert_eq!(digits, 4);

		let mut buf = [0u8; 256];
		let n = quercus_codec_decoder_transform_image_sequence_filename(
			name.as_ptr(),
			name.len(),
			42,
			buf.as_mut_ptr(),
			buf.len(),
		);
		assert!(n > 0);
		let out = std::str::from_utf8(&buf[..n as usize]).unwrap();
		assert_eq!(out, "frame_0042.png");

		// No trailing digits: not a sequence.
		let plain = "frame.png";
		assert_eq!(
			quercus_codec_decoder_image_sequence_digit_count(plain.as_ptr(), plain.len()),
			0
		);
		assert_eq!(
			quercus_codec_decoder_image_sequence_digit_count(std::ptr::null(), 0),
			-1
		);
	}
}
