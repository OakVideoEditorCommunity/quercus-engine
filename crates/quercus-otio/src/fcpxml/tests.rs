use super::*;
use std::error::Error as _;

#[test]
fn parse_time_forms() {
	assert_eq!(parse_time("0s").unwrap(), Rational::new(0, 1));
	assert_eq!(parse_time("48s").unwrap(), Rational::new(48, 1));
	assert_eq!(parse_time("100/3000s").unwrap(), Rational::new(1, 30));
	assert_eq!(
		parse_time("1001/30000s").unwrap(),
		Rational::new(1001, 30000)
	);
	assert_eq!(parse_time("1/24").unwrap(), Rational::new(1, 24));
	assert_eq!(parse_time("0").unwrap(), Rational::new(0, 1));
	assert_eq!(parse_time("1.5s").unwrap(), Rational::new(3, 2));
	assert!(parse_time("abc").is_err());
	assert!(parse_time("1/0s").is_err());
	assert!(parse_time("24f").is_err());
	assert!(parse_time("1/2/3s").is_err());
}

#[test]
fn format_time_forms() {
	assert_eq!(format_time(Rational::new(0, 1)), "0s");
	assert_eq!(format_time(Rational::new(48, 1)), "48s");
	assert_eq!(format_time(Rational::new(1001, 30000)), "1001/30000s");
	assert_eq!(format_time(Rational::new(2, 5)), "2/5s");
}

#[test]
fn ntsc_rate_round_trip() {
	// 1 frame at 29.97fps: frameDuration 1001/30000s -> 30000/1001 fps.
	let rate = rate_of(Rational::new(1001, 30000)).unwrap();
	assert_eq!(rate, Rational::new(30000, 1001));
	// 3 frames come back as an exact integer at that rate.
	let t = time_at_rate(Rational::new(3003, 30000), rate);
	assert!((t.value() - 3.0).abs() < 1e-9, "value = {}", t.value());
	// ... and the seconds round-trip to the same rational.
	assert_eq!(seconds_of(t), Rational::new(1001, 10000));
	assert_eq!(frame_duration_of(rate), Rational::new(1001, 30000));
	// 23.976 material (24000/1001) behaves the same.
	let rate23976 = rate_of(Rational::new(1001, 24000)).unwrap();
	assert_eq!(rate23976, Rational::new(24000, 1001));
	assert_eq!(frame_duration_of(rate23976), Rational::new(1001, 24000));
}

#[test]
fn version_gates() {
	assert!(check_version("1.0").is_ok());
	assert!(check_version("1.9").is_ok());
	assert!(check_version("1.10").is_ok());
	assert!(check_version("1.11").is_ok());
	assert!(check_version("2.0").is_err());
	assert!(check_version("0.9").is_err());
	assert!(check_version("1.12").is_err());
	assert!(check_version("bogus").is_err());
}

#[test]
fn format_names() {
	assert_eq!(
		format_name(Rational::new(1, 30)),
		"FFVideoFormat1920x1080p30"
	);
	assert_eq!(
		format_name(Rational::new(1001, 30000)),
		"FFVideoFormat1920x1080p29_97"
	);
}

#[test]
fn block_durations() {
	let mut clip = Clip::new("c");
	clip.set_source_range(TimeRange::new(
		RationalTime::new(0.0, 24.0),
		RationalTime::new(48.0, 24.0),
	));
	assert_eq!(block_duration(&Composable::Clip(clip)), Rational::new(2, 1));
	let mut t = Transition::new("t");
	t.set_in_offset(RationalTime::new(12.0, 24.0));
	t.set_out_offset(RationalTime::new(12.0, 24.0));
	assert_eq!(
		block_duration(&Composable::Transition(t)),
		Rational::new(1, 1)
	);
}

#[test]
fn error_display_and_source() {
	assert!(FcpxmlError::Xml("x".into())
		.to_string()
		.contains("XML error"));
	assert!(FcpxmlError::Malformed("x".into())
		.to_string()
		.contains("document error"));
	assert!(FcpxmlError::UnsupportedVersion("x".into())
		.to_string()
		.contains("version error"));
	let io = FcpxmlError::Io(std::io::Error::other("io"));
	assert!(io.to_string().contains("file error"));
	assert!(io.source().is_some());
	assert!(FcpxmlError::Xml("x".into()).source().is_none());
	assert!(FcpxmlError::Malformed("x".into()).source().is_none());
}

#[test]
fn negative_and_decimal_time_values() {
	assert_eq!(parse_time("-3/2s").unwrap(), Rational::new(-3, 2));
	assert_eq!(parse_time("-0.5s").unwrap(), Rational::new(-1, 2));
	assert_eq!(format_time(Rational::new(-3, 2)), "-3/2s");
}

#[test]
fn resource_error_branches_are_lenient() {
	let doc = r#"<fcpxml version="1.10">
	<resources>
		<format frameDuration="100/3000s"/>
		<format id="f2" frameDuration="bogus"/>
		<format id="f3" frameDuration="1/24s"/>
		<asset duration="junk"/>
		<asset id="a2"/>
	</resources>
	<library><event name="e"><project name="p">
		<sequence format="f3"><spine/></sequence>
	</project></event></library>
</fcpxml>"#;
	let timelines = from_fcpxml_string(doc).expect("broken resources are skipped");
	assert_eq!(timelines.len(), 1);
	assert_eq!(timelines[0].name(), "p");
}

#[test]
fn zero_frame_duration_is_an_error() {
	let doc = r#"<fcpxml version="1.10">
	<resources><format id="f1" frameDuration="0/3000s"/></resources>
	<library><event name="e"><project name="p">
		<sequence format="f1"><spine/></sequence>
	</project></event></library>
</fcpxml>"#;
	assert!(matches!(
		from_fcpxml_string(doc),
		Err(FcpxmlError::Malformed(_))
	));
}

#[test]
fn unknown_asset_format_falls_back_to_sequence_rate() {
	let doc = r#"<fcpxml version="1.10">
	<resources>
		<format id="f1" frameDuration="100/3000s"/>
		<asset id="a1" src="file:///x.mov" format="ghost" duration="10s"/>
	</resources>
	<library><event name="e"><project name="p">
		<sequence format="f1"><spine>
			<asset-clip name="c" ref="a1" offset="0s" duration="300/3000s" start="0s"/>
		</spine></sequence>
	</project></event></library>
</fcpxml>"#;
	let timelines = from_fcpxml_string(doc).expect("falls back to the sequence rate");
	let clip = timelines[0].tracks().children()[0]
		.as_track()
		.unwrap()
		.children()[0]
		.as_clip()
		.unwrap();
	let range = clip.source_range().unwrap();
	assert_eq!(range.duration().rate(), 30.0);
	// The asset reference still resolves (src is kept).
	let external = clip
		.media_reference()
		.unwrap()
		.as_external_reference()
		.unwrap();
	assert_eq!(external.target_url(), "file:///x.mov");
}

#[test]
fn nested_track_block_and_unknown_kind_are_skipped() {
	let mut timeline = Timeline::new("nested");
	let mut track = Track::new("Video");
	track.append_child(Composable::Track(Track::new("Video")));
	timeline.tracks_mut().append_child(Composable::Track(track));
	let xml = to_fcpxml_string(&[timeline]).expect("nested track block is skipped");
	assert!(xml.contains("<spine>"), "{xml}");
	assert!(xml.contains("</spine>"), "{xml}");

	let mut timeline = Timeline::new("kinds");
	let mut subtitles = Track::new("Subtitles");
	subtitles.append_child(Composable::Clip(Clip::new("c")));
	timeline
		.tracks_mut()
		.append_child(Composable::Track(subtitles));
	let xml = to_fcpxml_string(&[timeline]).expect("unknown track kind is skipped");
	assert!(!xml.contains("<spine>"), "{xml}");
	assert!(xml.contains("<sequence"), "{xml}");
}

#[test]
fn clip_without_media_reference_writes_without_ref() {
	let mut timeline = Timeline::new("t");
	let mut track = Track::new("Video");
	let mut clip = Clip::new("orphan");
	clip.set_source_range(TimeRange::new(
		RationalTime::new(0.0, 24.0),
		RationalTime::new(24.0, 24.0),
	));
	track.append_child(Composable::Clip(clip));
	timeline.tracks_mut().append_child(Composable::Track(track));
	let xml = to_fcpxml_string(&[timeline]).expect("export");
	assert!(
		xml.contains("<asset-clip name=\"orphan\" offset=\"0s\" duration=\"1s\" start=\"0s\"/>"),
		"{xml}"
	);
}

#[test]
fn file_io_errors() {
	assert!(from_fcpxml_file("/nonexistent/quercusotio-test.fcpxml").is_err());
	assert!(to_fcpxml_file(&[], std::env::temp_dir()).is_err());
}
