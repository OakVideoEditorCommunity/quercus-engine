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

//! Pixel and sample format enums (quercuscore `PixelFormat` /
//! `SampleFormat` equivalents). Integer values MUST match the C++
//! enums (`core/include/olive/core/render/pixelformat.h`,
//! `sampleformat.h`) — they cross the C ABI as `int`.

/// Pixel format (values identical to `olive::core::PixelFormat::Format`:
/// invalid=-1, u8=0, u10=1, u16=2, f16=3, f32=4).
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PixelFormat {
	/// Invalid/unspecified.
	Invalid = -1,
	/// 8-bit unsigned per channel.
	U8 = 0,
	/// 10-bit unsigned per channel (packed).
	U10 = 1,
	/// 16-bit unsigned per channel.
	U16 = 2,
	/// 16-bit half float.
	F16 = 3,
	/// 32-bit float (primary pipeline format).
	F32 = 4,
}

impl PixelFormat {
	/// Returns the storage size of one channel of this pixel format, in bytes.
	///
	/// `U8` is 1, `U16`/`F16` are 2, `F32` is 4, `Invalid` is 0, and `U10` is 4
	/// because it is packed RGBA10A2 stored as one 4-byte word per pixel (C++
	/// `PixelFormat::byte_count`). Counterpart of [`PixelFormat::bytes_per_pixel`],
	/// which multiplies this value by the channel count.
	pub fn bytes_per_channel(self) -> usize {
		match self {
			PixelFormat::Invalid => 0,
			PixelFormat::U8 => 1,
			PixelFormat::U10 => 4,
			PixelFormat::U16 | PixelFormat::F16 => 2,
			PixelFormat::F32 => 4,
		}
	}

	/// Returns the size of one pixel with `channels` channels, in bytes.
	///
	/// `channels` is the number of channels per pixel (4 for RGBA, 3 for RGB); the
	/// result is [`PixelFormat::bytes_per_channel`] multiplied by `channels`, so it
	/// is `0` for `Invalid` and may overflow for an absurd channel count. C++
	/// `PixelFormat::bytes_per_pixel`.
	pub fn bytes_per_pixel(self, channels: usize) -> usize {
		self.bytes_per_channel() * channels
	}
}

/// Audio sample format (values identical to
/// `olive::core::SampleFormat::Format`: PLANAR first — u8_p=0, s16_p=1,
/// s32_p=2, s64_p=3, f32_p=4, f64_p=5, then packed u8=6, s16=7, s32=8,
/// s64=9, f32=10, f64=11). This order is load-bearing: quercusaudio's
/// default format constant (f32_p) is 4.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SampleFormat {
	/// Invalid/unspecified.
	Invalid = -1,
	/// Unsigned 8-bit planar.
	U8Planar = 0,
	/// Signed 16-bit planar.
	S16Planar = 1,
	/// Signed 32-bit planar.
	S32Planar = 2,
	/// Signed 64-bit planar.
	S64Planar = 3,
	/// 32-bit float planar.
	F32Planar = 4,
	/// 64-bit float planar.
	F64Planar = 5,
	/// Unsigned 8-bit packed.
	U8 = 6,
	/// Signed 16-bit packed.
	S16 = 7,
	/// Signed 32-bit packed.
	S32 = 8,
	/// Signed 64-bit packed.
	S64 = 9,
	/// 32-bit float packed.
	F32 = 10,
	/// 64-bit float packed.
	F64 = 11,
}

impl SampleFormat {
	/// Returns the storage size of one sample of this format, in bytes.
	///
	/// A sample is one channel: 1 byte for `U8`/`U8Planar`, 2 for the `S16` pair, 4
	/// for the `S32`/`F32` pairs and 8 for the `S64`/`F64` pairs; `Invalid` returns
	/// `0`. Planar and packed variants of the same depth agree, so the result does
	/// not depend on the channel layout. C++ `SampleFormat::byte_count`.
	pub fn bytes_per_sample(self) -> usize {
		match self {
			SampleFormat::Invalid => 0,
			SampleFormat::U8Planar | SampleFormat::U8 => 1,
			SampleFormat::S16Planar | SampleFormat::S16 => 2,
			SampleFormat::S32Planar
			| SampleFormat::S32
			| SampleFormat::F32Planar
			| SampleFormat::F32 => 4,
			SampleFormat::S64Planar
			| SampleFormat::S64
			| SampleFormat::F64Planar
			| SampleFormat::F64 => 8,
		}
	}

	/// True when this format is planar, i.e. one plane per channel.
	///
	/// The planar formats are the first six discriminants (`0..=5`, `U8Planar`
	/// through `F64Planar`); `Invalid` (-1) and the packed formats (`U8`..`F64`,
	/// `6..=11`) return `false`. C++ `SampleFormat::is_planar` uses the same range
	/// test.
	pub fn is_planar(self) -> bool {
		(self as i32) >= 0 && (self as i32) < 6
	}

	/// Packed counterpart of a planar format (and vice versa;
	/// C++ `to_packed`/`to_planar`).
	pub fn to_packed(self) -> SampleFormat {
		match self {
			SampleFormat::U8Planar => SampleFormat::U8,
			SampleFormat::S16Planar => SampleFormat::S16,
			SampleFormat::S32Planar => SampleFormat::S32,
			SampleFormat::S64Planar => SampleFormat::S64,
			SampleFormat::F32Planar => SampleFormat::F32,
			SampleFormat::F64Planar => SampleFormat::F64,
			other => other,
		}
	}

	/// See [`SampleFormat::to_packed`].
	pub fn to_planar(self) -> SampleFormat {
		match self {
			SampleFormat::U8 => SampleFormat::U8Planar,
			SampleFormat::S16 => SampleFormat::S16Planar,
			SampleFormat::S32 => SampleFormat::S32Planar,
			SampleFormat::S64 => SampleFormat::S64Planar,
			SampleFormat::F32 => SampleFormat::F32Planar,
			SampleFormat::F64 => SampleFormat::F64Planar,
			other => other,
		}
	}
}
