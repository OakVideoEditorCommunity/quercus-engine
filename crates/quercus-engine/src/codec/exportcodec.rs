//! C ABI for `quercus_codec::exportcodec::Codec` (prefix `quercus_codec_codec_`).
//! Codecs cross the boundary as their `repr(i32)` discriminant values;
//! the C header defines the same enum.

use quercus_codec::exportcodec::Codec;

/// Reports whether `codec` is a real `Codec` discriminant.
///
/// `codec` is the `repr(i32)` value carried across the C ABI; the real codecs are
/// `0..=18`. The `Count` sentinel (19) and every negative or larger value answer
/// `false`, so a C caller can validate an untrusted value before using the other
/// `quercus_codec_codec_*` entry points. Pairs with
/// `quercus_codec_codec_count`, which returns the sentinel itself.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_codec_codec_is_valid(codec: i32) -> bool {
	Codec::from_i32(codec).is_some()
}

/// The number of real codecs (the `Count` sentinel value).
#[unsafe(no_mangle)]
pub extern "C" fn quercus_codec_codec_count() -> i32 {
	Codec::Count as i32
}

/// The display name of `codec`, snprintf-style (see `copy_str_out`);
/// -1 when `codec` is invalid.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_codec_codec_get_name(codec: i32, buf: *mut u8, buf_len: usize) -> i32 {
	let Some(codec) = Codec::from_i32(codec) else {
		return -1;
	};
	super::copy_str_out(&Codec::get_codec_name(codec), buf, buf_len)
}

/// Whether `codec` produces still images (OpenEXR/PNG/TIFF); false for
/// invalid values.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_codec_codec_is_still_image(codec: i32) -> bool {
	Codec::from_i32(codec).is_some_and(Codec::is_codec_a_still_image)
}

/// Reports whether `codec` encodes without quality loss.
///
/// `codec` is a `repr(i32)` `Codec` discriminant, answered from the codec table's
/// lossless flag (FLAC and PCM are lossless; H.264 is not). Invalid values answer
/// `false` instead of reporting an error, so this is safe to call before
/// `quercus_codec_codec_is_valid`. Sibling of
/// `quercus_codec_codec_is_still_image`.
#[unsafe(no_mangle)]
pub extern "C" fn quercus_codec_codec_is_lossless(codec: i32) -> bool {
	Codec::from_i32(codec).is_some_and(Codec::is_codec_lossless)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn validity_and_count() {
		assert!(quercus_codec_codec_is_valid(0));
		assert!(quercus_codec_codec_is_valid(18));
		assert!(!quercus_codec_codec_is_valid(19), "Count is not a real codec");
		assert!(!quercus_codec_codec_is_valid(-1));
		assert_eq!(quercus_codec_codec_count(), 19);
	}

	#[test]
	fn codec_name_roundtrip() {
		let mut buf = [0u8; 32];
		let n = quercus_codec_codec_get_name(1, buf.as_mut_ptr(), buf.len());
		assert_eq!(n, 5);
		assert_eq!(&buf[..6], b"H.264\0");

		// Invalid codec: -1, buffer untouched.
		buf[0] = b'x';
		assert_eq!(quercus_codec_codec_get_name(99, buf.as_mut_ptr(), buf.len()), -1);

		// Query mode still reports the required length.
		assert_eq!(quercus_codec_codec_get_name(12, std::ptr::null_mut(), 0), 3);
	}

	#[test]
	fn still_image_and_lossless_flags() {
		assert!(quercus_codec_codec_is_still_image(4)); // OpenEXR
		assert!(!quercus_codec_codec_is_still_image(1)); // H264
		assert!(!quercus_codec_codec_is_still_image(-1));
		assert!(quercus_codec_codec_is_lossless(16)); // FLAC
		assert!(!quercus_codec_codec_is_lossless(1));
	}
}
