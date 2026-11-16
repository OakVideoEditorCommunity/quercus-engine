use super::*;

/// The current platform has at least one hardware device candidate
/// (hardware decode is the mandated default on every supported
/// platform).
#[test]
fn platform_has_a_hardware_candidate() {
	assert!(
		!device_type_candidates().is_empty(),
		"no hardware decode candidate on this platform"
	);
}

/// Device types round-trip through their display names.
#[test]
fn device_type_names_are_stable() {
	assert_eq!(
		device_type_name(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX),
		"videotoolbox"
	);
	assert_eq!(
		device_type_name(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI),
		"vaapi"
	);
	assert_eq!(
		device_type_name(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA),
		"cuda/nvdec"
	);
	assert_eq!(
		device_type_name(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA),
		"d3d11va"
	);
}

/// Software and hardware pixel formats are classified correctly.
#[test]
fn hw_format_classification() {
	assert!(is_hw_format(sys::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX));
	assert!(is_hw_format(sys::AVPixelFormat::AV_PIX_FMT_VAAPI));
	assert!(is_hw_format(sys::AVPixelFormat::AV_PIX_FMT_CUDA));
	assert!(is_hw_format(sys::AVPixelFormat::AV_PIX_FMT_D3D11VA_VLD));
	assert!(!is_hw_format(sys::AVPixelFormat::AV_PIX_FMT_YUV420P));
	assert!(!is_hw_format(sys::AVPixelFormat::AV_PIX_FMT_NV12));
}

/// On macOS the VideoToolbox device context must be creatable (the
/// mandated default decode path) and the H.264 demo must open with
/// the hwaccel attached.
#[cfg(target_os = "macos")]
#[test]
fn videotoolbox_device_opens_for_h264() {
	let mut input =
		ffmpeg::format::input(&"../../testdata/demo.mp4".to_string()).expect("open demo.mp4");
	let fstream = input.stream(0).expect("stream 0");
	let params = fstream.parameters();
	let codec = ffmpeg::decoder::find(params.id()).expect("software h264 codec");
	let opened = open_hw_accel(
		&params,
		codec,
		sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
	);
	let Some((opened, _device)) = opened else {
		// Headless runners and FFmpeg builds without VideoToolbox support
		// use the mandated software fallback; there is no device to assert.
		return;
	};
	assert_eq!(
		unsafe { (*opened.as_ptr()).thread_count },
		1,
		"hardware decoding stays single-threaded"
	);
}

/// A device type marked unavailable is short-circuited before any
/// FFmpeg call: `open_hw_accel` returns None without attempting device
/// creation (which is what used to log
/// `[VAAPI @ ...] Failed to initialise VAAPI connection` every time on
/// boxes without the driver).
#[test]
fn negative_cache_skips_marked_device() {
	// Serialize with the media tests that hold the shared test lock
	// (the counter itself is thread-local, but the negative cache is
	// process-wide).
	let _lock = crate::lock_tests();
	// VDPAU is not a candidate on any supported platform, so marking
	// it cannot disturb the platform tests in this process (e.g. the
	// macOS VideoToolbox test above).
	let dev = sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU;
	mark_device_unavailable(dev);
	assert!(device_unavailable(dev));

	let input =
		ffmpeg::format::input(&"../../testdata/demo.mp4".to_string()).expect("open demo.mp4");
	let fstream = input.stream(0).expect("stream 0");
	let params = fstream.parameters();
	let codec = ffmpeg::decoder::find(params.id()).expect("software h264 codec");

	reset_creation_attempts();
	let opened = open_hw_accel(&params, codec, dev);
	assert!(opened.is_none(), "marked device must not open");
	assert_eq!(
		creation_attempts(),
		0,
		"marked device must be skipped before any creation attempt"
	);

	// Restore so the (non-candidate) type stays clean for other tests.
	UNAVAILABLE_DEVICES[dev as usize].store(false, std::sync::atomic::Ordering::Relaxed);
}

/// A device type never tried before is not cached as unavailable
/// (only a failed creation marks it). DXVA2 is not a candidate on any
/// supported platform (Windows uses D3D11VA/CUDA), so it is never
/// touched by other tests in this process.
#[test]
fn unmarked_device_type_is_not_unavailable() {
	let dev = sys::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2;
	assert!(!device_unavailable(dev));
}

#[cfg(target_os = "linux")]
#[test]
fn cuda_device_context_is_shared_by_reference() {
	let Some(mut first) = shared_cuda_device() else {
		eprintln!("no CUDA device; skipping shared-context test");
		return;
	};
	let Some(mut second) = shared_cuda_device() else {
		panic!("second CUDA device reference failed");
	};
	unsafe {
		assert_eq!((*first).buffer, (*second).buffer);
	}
	unsafe {
		sys::av_buffer_unref(&mut first);
		sys::av_buffer_unref(&mut second);
	}
}

/// L1: restarts are admitted inside the failure-streak budget; once the
/// streak is exhausted the per-frame fallback takes over instead of
/// pinning the whole stream.
#[test]
fn restart_budget_then_frame_fallback() {
	let mut health = HwStreamHealth::new();
	for _ in 0..(MAX_CONSECUTIVE_DECODE_FAILURES - 1) {
		health.note_decode_failure();
		assert!(
			health.admit_restart(),
			"a restart is admitted inside the budget"
		);
	}
	health.note_decode_failure();
	assert!(!health.admit_restart(), "the failure streak stops restarts");
	assert!(
		health.admit_frame_fallback(),
		"the frame fallback still applies"
	);
	assert!(
		!health.pinned(),
		"a burst of failures alone must not pin the stream"
	);
}

/// L1: the per-frame fallback budget is bounded; once spent, the caller
/// pins the stream.
#[test]
fn frame_fallback_budget_is_bounded() {
	let mut health = HwStreamHealth::new();
	for _ in 0..MAX_FRAME_FALLBACKS_PER_STREAM {
		assert!(health.admit_frame_fallback());
	}
	assert_eq!(health.frame_fallbacks(), MAX_FRAME_FALLBACKS_PER_STREAM);
	assert!(!health.admit_frame_fallback());
	health.pin();
	assert!(!health.admit_frame_fallback());
	assert!(!health.admit_restart());
	assert!(!health.hardware_allowed());
}

/// A decoded hardware frame clears the failure streak, so an isolated
/// reconfig (the VideoToolbox 4:2:2 10-bit case) does not exhaust the
/// retry budget.
#[test]
fn stream_health_success_resets_the_streak() {
	let mut health = HwStreamHealth::new();
	health.note_decode_failure();
	health.on_decode_success();
	assert_eq!(health.consecutive_failures(), 0);
	health.note_decode_failure();
	assert!(health.admit_restart());
	assert!(!health.pinned());
}

/// The restart budget rolls over after a healthy window, so a long healthy
/// session cannot accumulate restarts until it stops retrying.
#[test]
fn stream_health_restart_budget_rolls_over() {
	let mut health = HwStreamHealth::new();
	health.note_decode_failure();
	assert!(health.admit_restart());
	assert_eq!(health.session_restarts(), 1);
	for _ in 0..HW_RESTART_WINDOW_FRAMES {
		health.on_decode_success();
	}
	assert_eq!(
		health.session_restarts(),
		0,
		"the window resets the restart counter"
	);
	health.note_decode_failure();
	assert!(health.admit_restart());
}

/// L3: a format whose hardware attempts mostly fail is degraded, and the
/// degradation expires so the format is probed again.
#[test]
fn format_health_degrades_and_expires() {
	let _lock = crate::lock_tests();
	reset_format_health();
	// VDPAU is not a platform candidate, so this key cannot disturb the
	// real decode tests in this process.
	let key = HwFormatKey::new(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU, 27, 122, 0);
	for _ in 0..FORMAT_WINDOW_ATTEMPTS {
		note_format_attempt(&key, false);
	}
	assert!(format_degraded(&key));
	expire_format(&key);
	assert!(
		!format_degraded(&key),
		"an expired degradation must be probed again"
	);
	reset_format_health();
}

/// 4:2:2 10-bit heuristic: a stream whose hardware frames keep failing
/// the GPU import switches to software; other formats are unaffected.
#[test]
fn unimportable_hw_frames_prefer_software() {
	let _lock = crate::lock_tests();
	reset_format_health();
	let key = HwFormatKey::new(
		sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU,
		27,
		122,
		sys::AVPixelFormat::AV_PIX_FMT_YUV422P10LE as i32,
	);
	assert!(!prefer_software_for(&key));
	note_hw_import_unsupported(&key);
	assert!(!prefer_software_for(&key), "one failure is not enough");
	note_hw_import_unsupported(&key);
	assert!(
		prefer_software_for(&key),
		"repeated import failures flip the format"
	);

	// 4:2:0 8-bit hardware frames are not flipped by the heuristic.
	let other = HwFormatKey::new(
		sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU,
		27,
		100,
		sys::AVPixelFormat::AV_PIX_FMT_YUV420P as i32,
	);
	note_hw_import_unsupported(&other);
	note_hw_import_unsupported(&other);
	assert!(!prefer_software_for(&other));
	reset_format_health();
}

/// L3: a format whose hardware attempts mostly succeed is never degraded.
#[test]
fn format_health_keeps_healthy_format() {
	let _lock = crate::lock_tests();
	reset_format_health();
	let key = HwFormatKey::new(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2, 27, 100, 0);
	let half = FORMAT_WINDOW_ATTEMPTS / 2;
	for _ in 0..half {
		note_format_attempt(&key, false);
	}
	for _ in 0..(FORMAT_WINDOW_ATTEMPTS - half) {
		note_format_attempt(&key, true);
	}
	assert!(!format_degraded(&key));
	reset_format_health();
}
