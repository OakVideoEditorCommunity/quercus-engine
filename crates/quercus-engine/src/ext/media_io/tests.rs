use super::*;
use std::ffi::CString;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use quercus_codec::mediaio::provider_handles;

/// The C ABI has one process-wide provider; these tests must not
/// interleave with each other.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
	SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Drops any provider installed by a panicking test.
struct UnregisterOnDrop;

impl Drop for UnregisterOnDrop {
	fn drop(&mut self) {
		quercus_codec_mediaio_unregister();
		clear_provider();
	}
}

/// In-memory bytes served by the mock `open`.
const MOCK_BYTES: &[u8] = b"0123456789abcdef";

static CAN_OPEN_CALLS: AtomicUsize = AtomicUsize::new(0);
static OPEN_CALLS: AtomicUsize = AtomicUsize::new(0);
static READ_CALLS: AtomicUsize = AtomicUsize::new(0);
static SEEK_CALLS: AtomicUsize = AtomicUsize::new(0);
static CLOSE_CALLS: AtomicUsize = AtomicUsize::new(0);
static READ_FAILS: AtomicBool = AtomicBool::new(false);

fn reset_mock() {
	CAN_OPEN_CALLS.store(0, Ordering::SeqCst);
	OPEN_CALLS.store(0, Ordering::SeqCst);
	READ_CALLS.store(0, Ordering::SeqCst);
	SEEK_CALLS.store(0, Ordering::SeqCst);
	CLOSE_CALLS.store(0, Ordering::SeqCst);
	READ_FAILS.store(false, Ordering::SeqCst);
}

/// A mock "userdata" so the callbacks can prove they received the
/// opaque pointer unchanged.
fn mock_userdata() -> &'static AtomicUsize {
	static USERDATA: OnceLock<AtomicUsize> = OnceLock::new();
	USERDATA.get_or_init(|| AtomicUsize::new(0))
}

unsafe fn write_error(err: *mut QuercusMediaIoError, code: i32, message: &str) {
	let Some(err) = (unsafe { err.as_mut() }) else {
		return;
	};
	err.code = code;
	err.set_message(message);
}

/// Borrow a `(ptr, len)` path as UTF-8.
unsafe fn path_arg<'a>(ptr: *const u8, len: usize) -> Option<&'a str> {
	if ptr.is_null() {
		return None;
	}
	std::str::from_utf8(unsafe { std::slice::from_raw_parts(ptr, len) }).ok()
}

unsafe extern "C" fn mock_can_open(
	_ud: *mut c_void,
	path: *const u8,
	path_len: usize,
	_err: *mut QuercusMediaIoError,
) -> i32 {
	CAN_OPEN_CALLS.fetch_add(1, Ordering::SeqCst);
	let Some(path) = (unsafe { path_arg(path, path_len) }) else {
		return 0;
	};
	if path == "mem://negative" {
		return -7;
	}
	i32::from(path.starts_with("mem://"))
}

unsafe extern "C" fn mock_open(
	ud: *mut c_void,
	path: *const u8,
	path_len: usize,
	out_stream: *mut *mut c_void,
	err: *mut QuercusMediaIoError,
) -> i32 {
	OPEN_CALLS.fetch_add(1, Ordering::SeqCst);
	mock_userdata().store(ud as usize, Ordering::SeqCst);
	let Some(out_stream) = (unsafe { out_stream.as_mut() }) else {
		unsafe { write_error(err, E_INVALID, "mock: null out_stream") };
		return E_INVALID;
	};
	*out_stream = std::ptr::null_mut();
	let Some(path) = (unsafe { path_arg(path, path_len) }) else {
		unsafe { write_error(err, E_INVALID, "mock: non-UTF-8 path") };
		return E_INVALID;
	};
	match path {
		"mem://fail" => {
			unsafe { write_error(err, -3, "mock open denied") };
			-3
		}
		"mem://null" => 0,
		_ => {
			let cursor = Box::new(Cursor::new(MOCK_BYTES.to_vec()));
			*out_stream = Box::into_raw(cursor) as *mut c_void;
			0
		}
	}
}

unsafe extern "C" fn mock_read(
	_ud: *mut c_void,
	stream: *mut c_void,
	buf: *mut u8,
	cap: usize,
	err: *mut QuercusMediaIoError,
) -> i64 {
	READ_CALLS.fetch_add(1, Ordering::SeqCst);
	if stream.is_null() {
		unsafe { write_error(err, E_INVALID, "mock: null stream") };
		return -1;
	}
	if READ_FAILS.load(Ordering::SeqCst) {
		unsafe { write_error(err, -4, "mock read denied") };
		return -4;
	}
	let cursor = unsafe { &mut *(stream as *mut Cursor<Vec<u8>>) };
	let buf = if cap == 0 {
		&mut []
	} else {
		unsafe { std::slice::from_raw_parts_mut(buf, cap) }
	};
	match cursor.read(buf) {
		Ok(count) => count as i64,
		Err(error) => {
			unsafe { write_error(err, -5, &error.to_string()) };
			-5
		}
	}
}

unsafe extern "C" fn mock_seek(
	_ud: *mut c_void,
	stream: *mut c_void,
	offset: i64,
	whence: i32,
	err: *mut QuercusMediaIoError,
) -> i64 {
	SEEK_CALLS.fetch_add(1, Ordering::SeqCst);
	if stream.is_null() {
		unsafe { write_error(err, E_INVALID, "mock: null stream") };
		return -1;
	}
	let Some(from) = (match whence {
		0 => u64::try_from(offset).ok().map(SeekFrom::Start),
		1 => Some(SeekFrom::Current(offset)),
		2 => Some(SeekFrom::End(offset)),
		_ => None,
	}) else {
		unsafe { write_error(err, E_INVALID, "mock: bad seek") };
		return -1;
	};
	let cursor = unsafe { &mut *(stream as *mut Cursor<Vec<u8>>) };
	match cursor.seek(from) {
		Ok(position) => position as i64,
		Err(error) => {
			unsafe { write_error(err, -6, &error.to_string()) };
			-6
		}
	}
}

unsafe extern "C" fn mock_close(_ud: *mut c_void, stream: *mut c_void) {
	CLOSE_CALLS.fetch_add(1, Ordering::SeqCst);
	if stream.is_null() {
		return;
	}
	unsafe { drop(Box::from_raw(stream as *mut Cursor<Vec<u8>>)) };
}

fn mock_vtable(name: &CString) -> QuercusMediaIoVTable {
	QuercusMediaIoVTable {
		struct_size: std::mem::size_of::<QuercusMediaIoVTable>() as u32,
		abi_version: MEDIAIO_ABI_VERSION,
		name: name.as_ptr(),
		can_open: Some(mock_can_open),
		open: Some(mock_open),
		read: Some(mock_read),
		seek: Some(mock_seek),
		close: Some(mock_close),
	}
}

#[test]
fn invalid_vtables_are_rejected() {
	let _serial = serial();
	let _cleanup = UnregisterOnDrop;
	let name = CString::new("mock-mediaio").unwrap();

	assert_eq!(
		unsafe { quercus_codec_mediaio_register(std::ptr::null(), std::ptr::null_mut()) },
		E_INVALID
	);

	let mut vt = mock_vtable(&name);
	vt.struct_size -= 1;
	assert_eq!(
		unsafe { quercus_codec_mediaio_register(&vt, std::ptr::null_mut()) },
		E_INVALID,
		"a vtable that does not cover all fields must be rejected"
	);

	let mut vt = mock_vtable(&name);
	vt.abi_version = 2;
	assert_eq!(
		unsafe { quercus_codec_mediaio_register(&vt, std::ptr::null_mut()) },
		E_INVALID
	);

	let mut vt = mock_vtable(&name);
	vt.name = std::ptr::null();
	assert_eq!(
		unsafe { quercus_codec_mediaio_register(&vt, std::ptr::null_mut()) },
		E_INVALID
	);

	let empty = CString::new("").unwrap();
	let vt = mock_vtable(&empty);
	assert_eq!(
		unsafe { quercus_codec_mediaio_register(&vt, std::ptr::null_mut()) },
		E_INVALID,
		"an empty provider name must be rejected"
	);

	for missing in 0..5 {
		let mut vt = mock_vtable(&name);
		match missing {
			0 => vt.can_open = None,
			1 => vt.open = None,
			2 => vt.read = None,
			3 => vt.seek = None,
			_ => vt.close = None,
		}
		assert_eq!(
			unsafe { quercus_codec_mediaio_register(&vt, std::ptr::null_mut()) },
			E_INVALID,
			"missing callback {missing} must be rejected"
		);
	}

	assert!(!provider_handles("mem://x"), "nothing may be installed");
}

#[test]
fn register_unregister_and_can_open() {
	let _serial = serial();
	let _cleanup = UnregisterOnDrop;
	reset_mock();
	let name = CString::new("mock-mediaio").unwrap();
	let vt = mock_vtable(&name);
	let userdata = 0x5a5a_usize as *mut c_void;

	assert_eq!(unsafe { quercus_codec_mediaio_register(&vt, userdata) }, 0);
	assert!(provider_handles("mem://clip.wav"));
	assert!(!provider_handles("file:///tmp/clip.wav"));
	assert!(
		!provider_handles("mem://negative"),
		"a negative can_open result is a decline"
	);
	assert_eq!(CAN_OPEN_CALLS.load(Ordering::SeqCst), 3);

	// One process-wide provider: a second registration is rejected
	// and leaves the first installed.
	assert_eq!(
		unsafe { quercus_codec_mediaio_register(&vt, userdata) },
		E_INSTALLED
	);
	assert!(provider_handles("mem://clip.wav"));

	assert!(quercus_codec_mediaio_unregister());
	assert!(!provider_handles("mem://clip.wav"));
	assert!(
		!quercus_codec_mediaio_unregister(),
		"double unregister is false"
	);
}

#[test]
fn adapter_reads_seeks_and_closes() {
	let _serial = serial();
	let _cleanup = UnregisterOnDrop;
	reset_mock();
	let name = CString::new("mock-mediaio").unwrap();
	let vt = mock_vtable(&name);
	let userdata = 0x1234_usize as *mut c_void;
	assert_eq!(unsafe { quercus_codec_mediaio_register(&vt, userdata) }, 0);
	let provider = installed_provider_for_test().expect("the adapter is installed");

	assert!(MediaIoProvider::can_open(&*provider, "mem://bytes"));
	let mut stream = provider.open("mem://bytes").expect("open through adapter");
	assert_eq!(OPEN_CALLS.load(Ordering::SeqCst), 1);
	assert_eq!(
		mock_userdata().load(Ordering::SeqCst),
		userdata as usize,
		"the opaque userdata must reach the callback"
	);

	let mut buf = [0u8; 4];
	assert_eq!(stream.read(&mut buf).expect("read"), 4);
	assert_eq!(&buf, b"0123");

	assert_eq!(stream.seek(SeekFrom::Start(2)).expect("seek start"), 2);
	let mut pair = [0u8; 2];
	assert_eq!(stream.read(&mut pair).expect("read after seek"), 2);
	assert_eq!(&pair, b"23");

	assert_eq!(stream.seek(SeekFrom::End(-2)).expect("seek end"), 14);
	assert_eq!(
		stream.seek(SeekFrom::Current(-1)).expect("seek current"),
		13
	);
	assert_eq!(
		stream
			.seek(SeekFrom::Start(u64::MAX))
			.unwrap_err()
			.to_string(),
		"media stream seek offset too large"
	);

	// Provider errors carry their code and message through `open`.
	let error = provider.open("mem://fail").err().expect("mock open denial");
	assert_eq!(error.code, -3);
	assert!(error.message.contains("mock open denied"));
	let error = provider
		.open("mem://null")
		.err()
		.expect("a null handle is an error");
	assert_eq!(error.code, E_INVALID);
	assert!(error.message.contains("null stream"));

	// A failed read maps the error record to `io::Error::other`.
	READ_FAILS.store(true, Ordering::SeqCst);
	let error = stream.read(&mut buf).expect_err("mock read denial");
	assert!(error.to_string().contains("mock read denied"));
	READ_FAILS.store(false, Ordering::SeqCst);

	let closes = CLOSE_CALLS.load(Ordering::SeqCst);
	drop(stream);
	assert_eq!(
		CLOSE_CALLS.load(Ordering::SeqCst),
		closes + 1,
		"drop closes the host stream exactly once"
	);

	// Failed opens did not leak an active stream: unregister returns.
	assert!(quercus_codec_mediaio_unregister());
}

#[test]
fn unregister_waits_for_open_streams() {
	let _serial = serial();
	let _cleanup = UnregisterOnDrop;
	reset_mock();
	let name = CString::new("mock-mediaio").unwrap();
	let vt = mock_vtable(&name);
	assert_eq!(
		unsafe { quercus_codec_mediaio_register(&vt, std::ptr::null_mut()) },
		0
	);
	let provider = installed_provider_for_test().expect("the adapter is installed");
	let stream = provider
		.open("mem://bytes")
		.expect("open before unregister");

	let (started_tx, started_rx) = mpsc::channel();
	let (done_tx, done_rx) = mpsc::channel();
	let unregister = std::thread::spawn(move || {
		started_tx.send(()).expect("signal start");
		let removed = quercus_codec_mediaio_unregister();
		done_tx.send(removed).expect("signal completion");
	});
	started_rx
		.recv_timeout(Duration::from_secs(5))
		.expect("unregister thread started");

	// Wait until unregister has removed the provider from the codec
	// registry: from here on it can only be blocked in the stream
	// drain, so a timeout proves the wait is real.
	let deadline = Instant::now() + Duration::from_secs(5);
	while provider_handles("mem://bytes") {
		assert!(
			Instant::now() < deadline,
			"unregister did not remove the provider from the registry"
		);
		std::thread::sleep(Duration::from_millis(1));
	}

	// The stream is still open: unregister must block (no busy
	// spin, a condition variable) rather than return early.
	assert!(
		matches!(
			done_rx.recv_timeout(Duration::from_millis(100)),
			Err(mpsc::RecvTimeoutError::Timeout)
		),
		"unregister returned while a stream was still open"
	);
	assert_eq!(CLOSE_CALLS.load(Ordering::SeqCst), 0);

	drop(stream);
	assert!(
		done_rx
			.recv_timeout(Duration::from_secs(5))
			.expect("unregister returns after the stream closes")
	);
	unregister.join().expect("unregister thread");

	// Once unregistered, a retained adapter refuses new work without
	// calling the host again.
	let can_open_before = CAN_OPEN_CALLS.load(Ordering::SeqCst);
	assert!(!MediaIoProvider::can_open(&*provider, "mem://bytes"));
	assert_eq!(CAN_OPEN_CALLS.load(Ordering::SeqCst), can_open_before);
	let error = provider.open("mem://bytes").err().expect("closed provider");
	assert_eq!(error.code, E_INVALID);
}
