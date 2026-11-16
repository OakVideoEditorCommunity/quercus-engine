use super::*;
use std::sync::Mutex;

// The task-submit registry is process-global; serialize the tests
// that touch it.
static SERIAL: Mutex<()> = Mutex::new(());

fn proxy_task<'a>(input: &'a str, output: &'a str) -> TaskRequest<'a> {
	TaskRequest {
		kind: TaskKind::Proxy,
		input_filename: input,
		output_filename: output,
		stream_index: 3,
		sample_rate: 0,
		channel_layout: 0,
		sample_format: 0,
		proxy_width: 640,
		proxy_height: 360,
	}
}

fn temp_cache(name: &str) -> String {
	let dir = std::env::temp_dir().join(format!(
		"quercusengine_sdk_proxy_{name}_{}",
		std::process::id()
	));
	let _ = std::fs::create_dir_all(&dir);
	dir.to_string_lossy().into_owned()
}

#[test]
fn codec_enum_roundtrip_and_flags() {
	assert_eq!(Codec::count(), 19);
	assert_eq!(Codec::from_i32(1), Some(Codec::H264));
	assert_eq!(Codec::from_i32(19), None);
	assert_eq!(Codec::from_i32(-1), None);
	assert_eq!(Codec::H264.name(), "H.264");
	assert!(Codec::OpenEXR.is_still_image());
	assert!(!Codec::H264.is_still_image());
	assert!(Codec::FLAC.is_lossless());
}

#[test]
fn format_strings_and_codec_lists() {
	assert_eq!(Format::count(), 15);
	assert_eq!(Format::from_i32(1), Some(Format::Matroska));
	assert_eq!(Format::from_i32(15), None);
	assert_eq!(Format::Matroska.name(), "Matroska Video");
	assert_eq!(Format::Matroska.extension(), "mkv");

	let video = Format::Matroska.video_codecs();
	assert!(video.to_vec().contains(&(Codec::H264 as i32)));
	let audio = Format::Matroska.audio_codecs();
	assert!(audio.to_vec().contains(&(Codec::AAC as i32)));
}

#[test]
fn proxy_params_roundtrip_and_validation() {
	let mut p = ProxyParams::default();
	assert_eq!((p.width, p.height, p.divider), (1280, 720, 1));
	assert_eq!(p.extension(), "mp4");
	assert_eq!(p.preset(), "veryfast");

	p.set_extension("mkv").unwrap();
	assert_eq!(p.extension(), "mkv");
	assert!(p.set_extension(&"x".repeat(32)).is_err());
	assert!(p.set_preset("a\0b").is_err());
}

#[test]
fn proxy_state_and_filenames() {
	assert_eq!(
		proxy_get_state("/nonexistent/quercus-sdk-proxy.mp4"),
		ProxyState::Missing
	);
	let params = ProxyParams::default();
	let cache = std::env::temp_dir().to_string_lossy().into_owned();
	let filename = proxy_get_filename(&cache, "quercus-sdk-src.mp4", 0, &params).expect("filename");
	assert!(filename.ends_with(".v1.a1.mp4"), "unexpected: {filename}");
	let working = proxy_get_working_filename(&filename).expect("working");
	assert!(working.ends_with(".working.mp4"));
}

#[test]
fn proxy_get_or_start_states() {
	let _guard = SERIAL.lock().unwrap();
	set_task_submit_cb::<fn(&TaskRequest) -> i32>(None);

	let cache = temp_cache("missing");
	let result = proxy_get_or_start(
		&cache,
		"/tmp/quercus-sdk-gos.mp4",
		0,
		&ProxyParams::default(),
	)
	.expect("call");
	assert_eq!(result.state, ProxyState::Missing);
	assert!(!result.filename.is_empty());

	// A registered, accepting callback flips it to Generating.
	set_task_submit_cb(Some(|_req: &TaskRequest| 0));
	let cache = temp_cache("generating");
	let result = proxy_get_or_start(
		&cache,
		"/tmp/quercus-sdk-gos.mp4",
		0,
		&ProxyParams::default(),
	)
	.expect("call");
	assert_eq!(result.state, ProxyState::Generating);
	set_task_submit_cb::<fn(&TaskRequest) -> i32>(None);
}

#[test]
fn task_submit_without_and_with_registrar() {
	let _guard = SERIAL.lock().unwrap();
	set_task_submit_cb::<fn(&TaskRequest) -> i32>(None);
	assert!(!task_submit_is_registered());
	assert_eq!(
		task_submit(&proxy_task("/tmp/a.mp4", "/tmp/a.p.mp4")),
		Ok(false)
	);

	use std::sync::atomic::{AtomicUsize, Ordering};
	static CALLS: AtomicUsize = AtomicUsize::new(0);
	CALLS.store(0, Ordering::SeqCst);
	set_task_submit_cb(Some(move |req: &TaskRequest| {
		CALLS.fetch_add(1, Ordering::SeqCst);
		assert_eq!(req.kind, TaskKind::Proxy);
		assert_eq!(req.input_filename, "/tmp/a.mp4");
		assert_eq!(req.stream_index, 3);
		0
	}));
	assert!(task_submit_is_registered());
	assert_eq!(
		task_submit(&proxy_task("/tmp/a.mp4", "/tmp/a.p.mp4")),
		Ok(true)
	);
	assert_eq!(CALLS.load(Ordering::SeqCst), 1);

	set_task_submit_cb::<fn(&TaskRequest) -> i32>(None);
	assert!(!task_submit_is_registered());
}

#[test]
fn task_submit_rejects_nul_filenames() {
	let _guard = SERIAL.lock().unwrap();
	set_task_submit_cb::<fn(&TaskRequest) -> i32>(None);
	let req = proxy_task("/tmp/a\0b.mp4", "/tmp/a.p.mp4");
	assert!(task_submit(&req).is_err());
}

#[test]
fn decoder_ids_and_sequence_helpers() {
	let ids = decoder_ids();
	assert!(ids.iter().any(|id| id == "ffmpeg"));

	assert_eq!(image_sequence_digit_count("frame_0001.png"), Some(4));
	assert_eq!(image_sequence_digit_count("frame.png"), Some(0));
	assert_eq!(
		transform_image_sequence_filename("frame_0001.png", 42).as_deref(),
		Some("frame_0042.png")
	);
}

#[test]
fn encoder_placeholder_helpers() {
	assert!(filename_contains_digit_placeholder("out_[#####].png"));
	assert!(!filename_contains_digit_placeholder("out.png"));
	assert_eq!(placeholder_digit_count("out_[#####].png"), Some(5));
	let stripped = remove_digit_placeholder("out_[#####].png").expect("strip");
	assert!(!stripped.contains('#'), "got {stripped}");
}

#[test]
fn probe_negative_paths() {
	// The positive path is covered end-to-end by the engine tests
	// (they generate real media with testmedia); here we verify the
	// wrapper's None mapping.
	assert!(decoder_probe(Some("no-such-decoder"), "/nonexistent/x.mp4", None).is_none());
	assert!(decoder_probe(None, "/nonexistent/x.mp4", None).is_none());
}

#[test]
fn decoder_session_negative_paths() {
	// The positive decode path is covered end-to-end by the engine
	// tests (they generate real media with testmedia).
	assert!(DecoderSession::open(Some("no-such-decoder"), "/nonexistent/x.mp4", 0).is_none());
	assert!(DecoderSession::open(None, "/nonexistent/x.mp4", 0).is_none());
}

#[test]
fn retrieve_params_builder_chains() {
	use crate::types::{Rational, TimeRange};
	let p = RetrieveParams::new()
		.stream("clip.mp4", 0)
		.time(Rational::new(1, 2))
		.length(TimeRange::new(Rational::new(0, 1), Rational::new(10, 1)))
		.force_range(Some(1))
		.force_range(None)
		.image_sequence(4, 7)
		.target_size(Some((1920, 1080)))
		.target_size(None)
		.mode(true)
		.premultiplied_alpha(true);
	let clone = p.clone();
	drop(p);
	drop(clone);
}

#[test]
fn encode_h264_roundtrip_via_sdk() {
	let out = std::env::temp_dir().join(format!("quercussdk_encode_{}.mp4", std::process::id()));
	let mut params = QuercusCodecEncodingParams::new();
	params.set_filename(out.to_str().unwrap()).unwrap();
	params.format = Format::MPEG4Video as i32;
	params.video_enabled = 1;
	params.video_codec = Codec::H264 as i32;
	params.video_width = 64;
	params.video_height = 64;
	params.video_time_base_num = 1;
	params.video_time_base_den = 10;
	params.video_pixel_format = crate::types::PixelFormat::F32 as i32;
	params.video_pixel_aspect_num = 1;
	params.video_pixel_aspect_den = 1;

	let session = EncoderSession::create(&params).expect("create");
	session.open().expect("open");

	let mut vp = crate::types::VideoParams::new();
	vp.width = 64;
	vp.height = 64;
	vp.format = crate::types::PixelFormat::F32 as i32;
	vp.channel_count = 4;
	for i in 0..10 {
		let mut frame = Frame::with_params(&vp);
		frame.allocate().expect("allocate");
		frame.set_timestamp(crate::types::Rational::new(i, 10));
		frame.data().unwrap()[..64 * 16].fill(0x3F); // some nonzero pattern
		session.write_video(&frame).expect("write");
	}
	session.flush().expect("flush");
	session.close().expect("close");
	drop(session);

	let len = std::fs::metadata(&out).expect("output exists").len();
	assert!(len > 1000, "encoded file is implausibly small: {len}");
	let _ = std::fs::remove_file(&out);
}

#[test]
fn encoder_negative_paths() {
	// Unknown format.
	let params = QuercusCodecEncodingParams::new();
	assert!(EncoderSession::create(&params).is_none());

	// Field validation.
	let mut params = QuercusCodecEncodingParams::new();
	assert!(params.set_filename(&"x".repeat(2000)).is_err());
	assert!(params.set_filename("a\0b").is_err());
	params.set_filename("/tmp/quercus.mp4").unwrap();
	assert_eq!(params.filename(), "/tmp/quercus.mp4");
}

#[test]
fn frame_lifecycle() {
	let mut params = crate::types::VideoParams::new();
	params.width = 64;
	params.height = 32;
	params.format = crate::types::PixelFormat::F32 as i32;
	params.channel_count = 4;

	let mut frame = Frame::with_params(&params);
	assert!(!frame.is_allocated());
	frame.allocate().expect("allocate");
	assert_eq!((frame.width(), frame.height()), (64, 32));
	assert_eq!(frame.format(), crate::types::PixelFormat::F32);
	assert_eq!(frame.allocated_size(), 64 * 32 * 4 * 4);
	assert_eq!(frame.linesize_bytes(), 64 * 4 * 4);

	frame.data().expect("data")[..16].fill(0xAB);
	frame.set_timestamp(crate::types::Rational::new(3, 2));
	assert_eq!(frame.timestamp(), crate::types::Rational::new(3, 2));
	assert_eq!(frame.params().map(|p| p.width), Some(64));

	// Clones share the buffer until the last drop.
	let clone = frame.clone();
	drop(frame);
	assert!(clone.is_allocated());
}
