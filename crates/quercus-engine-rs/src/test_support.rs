//! Test-only media fixtures built through the public SDK.

use crate::codec::{Codec, EncoderSession, EncodingParams, Format, Frame};
use crate::types::{PixelFormat, Rational, VideoParams};

/// A real 64x64, ten-frame H.264 clip with no app-side dependencies.
pub(crate) fn make_test_clip(name: &str) -> String {
	make_test_clip_impl(name, false)
}

/// A real H.264 clip with one second of stereo 440 Hz PCM audio.
pub(crate) fn make_test_clip_with_audio(name: &str) -> String {
	make_test_clip_impl(name, true)
}

/// Encode a throwaway H.264 MP4 fixture into the OS temp directory and
/// return its path.
///
/// The file is `quercussdk_editor_fixture_{name}_{pid}.mp4` in
/// [`std::env::temp_dir`]; the process id keeps parallel test binaries
/// apart, but the file is neither truncated nor removed here, so each
/// call overwrites the same per-process path. The video track is
/// always present: 64x64, [`PixelFormat::F32`], four channels, ten
/// frames timestamped `0..10/10` (1/10 s time base). `name` therefore
/// only affects the filename — it is used verbatim, with no escaping,
/// so characters illegal in a path component will fail the encode.
///
/// `with_audio` adds one second of stereo `440 Hz` sine at 48 kHz,
/// encoded as [`Codec::PCM`] with the left/right channel layout, and
/// written after the video frames. It is the backing implementation of
/// [`make_test_clip`] (`false`) and [`make_test_clip_with_audio`]
/// (`true`); tests using it need a built H.264 encoder.
///
/// # Panics
///
/// Panics with `create encoder`, `open encoder`, `allocate frame`,
/// `write frame`, `write audio`, `flush encoder` or `close encoder`
/// when the corresponding SDK step fails, and when the temp path is
/// not valid UTF-8.
fn make_test_clip_impl(name: &str, with_audio: bool) -> String {
	let out = std::env::temp_dir().join(format!(
		"quercussdk_editor_fixture_{name}_{}.mp4",
		std::process::id()
	));
	let mut params = EncodingParams::new();
	params.set_filename(out.to_str().unwrap()).unwrap();
	params.format = Format::MPEG4Video as i32;
	params.video_enabled = 1;
	params.video_codec = Codec::H264 as i32;
	params.video_width = 64;
	params.video_height = 64;
	params.video_time_base_num = 1;
	params.video_time_base_den = 10;
	params.video_pixel_format = PixelFormat::F32 as i32;
	params.video_pixel_aspect_num = 1;
	params.video_pixel_aspect_den = 1;
	if with_audio {
		params.audio_enabled = 1;
		params.audio_codec = Codec::PCM as i32;
		params.audio_sample_rate = 48_000;
		params.audio_channel_layout = 0x3;
		params.audio_sample_format = crate::audio::SampleFormat::F32 as i32;
	}

	let session = EncoderSession::create(&params).expect("create encoder");
	session.open().expect("open encoder");
	let mut video = VideoParams::new();
	video.width = 64;
	video.height = 64;
	video.format = PixelFormat::F32 as i32;
	video.channel_count = 4;
	for index in 0..10 {
		let mut frame = Frame::with_params(&video);
		frame.allocate().expect("allocate frame");
		frame.set_timestamp(Rational::new(index, 10));
		session.write_video(&frame).expect("write frame");
	}
	if with_audio {
		let mut samples = Vec::with_capacity(96_000);
		for index in 0..48_000 {
			let sample = (index as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
			samples.extend_from_slice(&[sample, sample]);
		}
		session.write_audio(&samples, 48_000).expect("write audio");
	}
	session.flush().expect("flush encoder");
	session.close().expect("close encoder");
	out.to_string_lossy().into_owned()
}

/// Builds a scan directory containing the bundled CPU OFX test plugin.
/// Returns `None` when the build artifact is unavailable.
pub(crate) fn ofx_plugin_scan_dir() -> Option<std::path::PathBuf> {
	let profile_dir = std::path::Path::new(env!("OUT_DIR")).ancestors().nth(3)?.to_path_buf();
	let build = profile_dir.join("build");
	let mut library = None;
	for entry in std::fs::read_dir(&build).ok()? {
		let entry = entry.ok()?;
		if !entry.file_name().to_string_lossy().starts_with("quercus-ofx-plugin-") {
			continue;
		}
		for artifact in ["quercus_test_plugin.so", "quercus_test_plugin.dylib"] {
			let candidate = entry.path().join("out").join(artifact);
			if candidate.exists() {
				library = Some(candidate);
				break;
			}
		}
		if library.is_some() {
			break;
		}
	}
	let library = library?;
	let bundle = std::env::temp_dir().join("quercus-sdk-ofx-test/quercus_test_plugin.ofx.bundle");
	let platform = if cfg!(target_os = "macos") {
		"MacOS"
	} else if cfg!(target_os = "windows") {
		"Win64"
	} else {
		"Linux-x86-64"
	};
	let bin_dir = bundle.join("Contents").join(platform);
	std::fs::create_dir_all(&bin_dir).ok()?;
	let target = bin_dir.join(if cfg!(target_os = "windows") { "plugin.dll" } else { "plugin" });
	if !target.exists() {
		std::fs::copy(&library, &target).ok()?;
	}
	bundle.parent().map(|path| path.to_path_buf())
}
