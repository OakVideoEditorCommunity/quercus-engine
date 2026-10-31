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

//! Custom media byte sources for FFmpeg inputs.
//!
//! A host (object store, proxy cache, vendor SDK) installs one process-wide
//! [`MediaIoProvider`]. Whenever FFmpeg opens a URI that provider claims via
//! [`MediaIoProvider::can_open`], the bytes are read through the provider's
//! [`MediaStream`] instead of a local path. Opening goes through
//! [`open_input`], which the decoder and probe paths use; an unclaimed path
//! keeps the ordinary local-file behavior.
//!
//! The provider is global because FFmpeg's format context carries no
//! host-owned Rust state of its own. Concurrency: the registry is only
//! locked long enough to clone the [`Arc`]; [`MediaIoProvider::can_open`]
//! and [`MediaIoProvider::open`] run outside the lock.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex, OnceLock};

use ffmpeg_next as ffmpeg;

/// Failure reported by a [`MediaIoProvider`] or [`MediaStream`] when opening
/// a source.
///
/// `code` is provider-defined (usually a POSIX `errno` or a vendor status);
/// `message` is human-readable. [`open_input`] maps the error to FFmpeg's
/// `EIO` and keeps the message for diagnostics, since FFmpeg's API has no
/// slot for arbitrary provider errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaIoError {
	/// Provider-defined status code (0 means "unspecified").
	pub code: i32,
	/// Human-readable diagnostic.
	pub message: String,
}

impl MediaIoError {
	/// Build an error from a status code and message.
	pub fn new(code: i32, message: impl Into<String>) -> Self {
		MediaIoError {
			code,
			message: message.into(),
		}
	}
}

impl std::fmt::Display for MediaIoError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{} (code {})", self.message, self.code)
	}
}

impl std::error::Error for MediaIoError {}

/// Readable and seekable byte source for one media resource.
///
/// Implementations must be `Send` (FFmpeg may open and drive the stream from
/// a decoder/probe thread) but need not be `Sync`. `read` and `seek` follow
/// [`std::io::Read`] / [`std::io::Seek`] semantics: short reads are allowed,
/// `seek` returns the new absolute position, and an unsupported operation
/// should report [`io::ErrorKind::Unsupported`].
pub trait MediaStream: Send {
	/// Read up to `buf.len()` bytes, returning 0 only at end of stream.
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;

	/// Seek relative to the start, current position or end of the stream.
	fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;

	/// Total length in bytes when known; used for streaming/seek support
	/// checks. The default reports [`io::ErrorKind::Unsupported`].
	fn len(&mut self) -> io::Result<u64> {
		Err(io::ErrorKind::Unsupported.into())
	}
}

/// Host-supplied resolver for custom media URIs.
///
/// A provider is installed process-wide with [`install_provider`]. `can_open`
/// must be cheap and I/O-free (it is called for every input open, including
/// ordinary local files); `open` performs the actual lookup/fetch and returns
/// a fresh seekable stream per call. Implementations must be `Send + Sync`:
/// the registry is shared across decoder and probe threads.
pub trait MediaIoProvider: Send + Sync {
	/// Short provider name, used in diagnostics.
	fn name(&self) -> &str;

	/// Cheap, I/O-free claim check on the URI/path text.
	fn can_open(&self, path: &str) -> bool;

	/// Open `path`, returning a seekable byte stream.
	fn open(&self, path: &str) -> Result<Box<dyn MediaStream>, MediaIoError>;
}

/// The installed provider slot. Guarded so installation/clearing can happen
/// while another thread opens media.
static PROVIDER: OnceLock<Mutex<Option<Arc<dyn MediaIoProvider>>>> = OnceLock::new();

/// The provider slot, initialized on first use.
fn provider_slot() -> &'static Mutex<Option<Arc<dyn MediaIoProvider>>> {
	PROVIDER.get_or_init(|| Mutex::new(None))
}

/// Clone the installed provider out of the registry. The registry lock is
/// dropped before the caller invokes any provider method.
fn current_provider() -> Option<Arc<dyn MediaIoProvider>> {
	provider_slot()
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner())
		.clone()
}

/// Install `provider`, replacing any previously installed provider.
///
/// Returns the previous provider, if any. Dropping that `Arc` (rather than
/// holding it) lets the provider's streams be released promptly.
pub fn install_provider(provider: Arc<dyn MediaIoProvider>) -> Option<Arc<dyn MediaIoProvider>> {
	provider_slot()
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner())
		.replace(provider)
}

/// Remove the installed provider, returning it if one was installed.
pub fn clear_provider() -> Option<Arc<dyn MediaIoProvider>> {
	provider_slot()
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner())
		.take()
}

/// Whether a provider is installed and claims `path`.
pub fn provider_handles(path: &str) -> bool {
	current_provider().is_some_and(|provider| provider.can_open(path))
}

/// Adapter from [`MediaStream`] to the [`Read`] + [`Seek`] pair that
/// [`ffmpeg::format::StreamIo::from_read_seek`] requires.
struct StreamAdapter {
	inner: Box<dyn MediaStream>,
}

impl Read for StreamAdapter {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		self.inner.read(buf)
	}
}

impl Seek for StreamAdapter {
	fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
		self.inner.seek(pos)
	}
}

/// Open `filename` for FFmpeg input, routing through the installed
/// [`MediaIoProvider`] when it claims the path and falling back to
/// `input_with_dictionary` otherwise.
///
/// Provider failures are mapped to [`ffmpeg::Error::Other`] with `errno`
/// `EIO`; the provider's message is written to stderr (FFmpeg's error type
/// cannot carry it). An `options` dictionary, when given, is consumed by
/// either path exactly as FFmpeg consumes it.
pub(crate) fn open_input(
	filename: &str,
	options: Option<ffmpeg::Dictionary>,
) -> Result<ffmpeg::format::context::Input, ffmpeg::Error> {
	if provider_handles(filename) {
		if let Some(provider) = current_provider() {
			let stream = provider.open(filename).map_err(|error| {
				eprintln!(
					"mediaio: provider '{}' could not open '{}': {}",
					provider.name(),
					filename,
					error
				);
				ffmpeg::Error::Other {
					errno: ffmpeg::error::EIO,
				}
			})?;
			let stream_io =
				ffmpeg::format::context::StreamIo::from_read_seek(StreamAdapter { inner: stream })
					.map_err(|error| {
						eprintln!(
							"mediaio: could not wrap the stream for '{}': {}",
							filename, error
						);
						error
					})?;
			return ffmpeg::format::input_from_stream(stream_io, Some(filename), options);
		}
	}

	ffmpeg::format::input_with_dictionary(filename, options.unwrap_or_else(ffmpeg::Dictionary::new))
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::io::Cursor;
	use std::sync::Mutex;

	/// Serializes tests that install/clear the process-wide provider.
	static IO_TEST_LOCK: Mutex<()> = Mutex::new(());

	/// Clears the provider even when an assertion panics.
	struct ClearOnDrop;

	impl Drop for ClearOnDrop {
		fn drop(&mut self) {
			clear_provider();
		}
	}

	fn lock_io_tests() -> std::sync::MutexGuard<'static, ()> {
		IO_TEST_LOCK
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner())
	}

	/// In-memory [`MediaStream`] over a byte vector.
	struct CursorStream {
		cursor: Cursor<Vec<u8>>,
	}

	impl MediaStream for CursorStream {
		fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
			self.cursor.read(buf)
		}

		fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
			self.cursor.seek(pos)
		}

		fn len(&mut self) -> io::Result<u64> {
			Ok(self.cursor.get_ref().len() as u64)
		}
	}

	/// Provider that claims one URI scheme and serves `bytes`.
	struct FakeProvider {
		scheme: &'static str,
		bytes: Vec<u8>,
	}

	impl FakeProvider {
		fn new(scheme: &'static str, bytes: Vec<u8>) -> Self {
			FakeProvider { scheme, bytes }
		}
	}

	impl MediaIoProvider for FakeProvider {
		fn name(&self) -> &str {
			"fake"
		}

		fn can_open(&self, path: &str) -> bool {
			path.starts_with(self.scheme)
		}

		fn open(&self, _path: &str) -> Result<Box<dyn MediaStream>, MediaIoError> {
			Ok(Box::new(CursorStream {
				cursor: Cursor::new(self.bytes.clone()),
			}))
		}
	}

	/// Minimal RIFF/WAVE PCM 16-bit mono 8 kHz clip (8 samples).
	fn wav_bytes() -> Vec<u8> {
		const SAMPLES: [i16; 8] = [0, 4096, 8192, 4096, 0, -4096, -8192, -4096];
		let data_len = (SAMPLES.len() * 2) as u32;
		let mut bytes = Vec::with_capacity(44 + data_len as usize);
		bytes.extend_from_slice(b"RIFF");
		bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
		bytes.extend_from_slice(b"WAVE");
		bytes.extend_from_slice(b"fmt ");
		bytes.extend_from_slice(&16u32.to_le_bytes());
		bytes.extend_from_slice(&1u16.to_le_bytes());
		bytes.extend_from_slice(&1u16.to_le_bytes());
		bytes.extend_from_slice(&8000u32.to_le_bytes());
		bytes.extend_from_slice(&16000u32.to_le_bytes());
		bytes.extend_from_slice(&2u16.to_le_bytes());
		bytes.extend_from_slice(&16u16.to_le_bytes());
		bytes.extend_from_slice(b"data");
		bytes.extend_from_slice(&data_len.to_le_bytes());
		for sample in SAMPLES {
			bytes.extend_from_slice(&sample.to_le_bytes());
		}
		bytes
	}

	#[test]
	fn stream_adapter_reads_and_seeks() {
		let mut adapter = StreamAdapter {
			inner: Box::new(CursorStream {
				cursor: Cursor::new(b"abcdef".to_vec()),
			}),
		};

		let mut buf = [0u8; 3];
		adapter.read_exact(&mut buf).expect("read at start");
		assert_eq!(&buf, b"abc");

		assert_eq!(adapter.seek(SeekFrom::Start(1)).expect("seek start"), 1);
		adapter.read_exact(&mut buf).expect("read after start seek");
		assert_eq!(&buf, b"bcd");

		assert_eq!(adapter.seek(SeekFrom::End(-2)).expect("seek end"), 4);
		adapter
			.read_exact(&mut buf[..2])
			.expect("read after end seek");
		assert_eq!(&buf[..2], b"ef");

		assert_eq!(
			adapter.seek(SeekFrom::Current(-1)).expect("seek current"),
			5
		);
		adapter
			.read_exact(&mut buf[..1])
			.expect("read after current seek");
		assert_eq!(&buf[..1], b"f");
	}

	#[test]
	fn provider_install_replace_and_clear() {
		let _lock = lock_io_tests();
		let _cleanup = ClearOnDrop;
		clear_provider();

		let first: Arc<dyn MediaIoProvider> = Arc::new(FakeProvider::new("first://", Vec::new()));
		assert!(install_provider(first.clone()).is_none());
		assert!(provider_handles("first://clip"));
		assert!(!provider_handles("second://clip"));

		let second: Arc<dyn MediaIoProvider> = Arc::new(FakeProvider::new("second://", Vec::new()));
		let previous = install_provider(second.clone()).expect("previous provider");
		assert!(Arc::ptr_eq(&previous, &first));
		assert!(provider_handles("second://clip"));
		assert!(!provider_handles("first://clip"));

		let previous = clear_provider().expect("installed provider");
		assert!(Arc::ptr_eq(&previous, &second));
		assert!(!provider_handles("second://clip"));
		assert!(clear_provider().is_none());
	}

	#[test]
	fn open_input_routes_through_provider() {
		let _lock = lock_io_tests();
		let _cleanup = ClearOnDrop;
		clear_provider();

		install_provider(Arc::new(FakeProvider::new("mem://", wav_bytes())));
		assert!(provider_handles("mem://tone.wav"));
		// A custom source must not be treated as missing by the local check.
		assert!(
			crate::decoder::CodecStream::with_block("mem://tone.wav".to_string(), 0, None).exists()
		);

		let input = open_input("mem://tone.wav", None).expect("open WAV through provider");
		assert!(input.streams().best(ffmpeg::media::Type::Audio).is_some());
		assert_eq!(input.nb_streams(), 1);
	}
}
