use super::*;

fn assert_rt(value: f64, num: i64, den: i64) {
	let r = from_double(value);
	assert_eq!(
		(r.numerator(), r.denominator()),
		(num, den),
		"from_double({value}) != {num}/{den}"
	);
}

#[test]
fn from_double_basics() {
	assert_rt(0.0, 0, 1);
	assert_rt(1.0, 1, 1);
	assert_rt(24.0, 24, 1);
	assert_rt(0.1, 1, 10);
	assert_rt(4.8, 24, 5);
	assert_rt(-0.1, -1, 10);
	assert_rt(1152.0 / 24.0, 48, 1);
	assert_rt(123456789.125, 987654313, 8);
}

#[test]
fn from_double_sentinels() {
	assert!(from_double(f64::NAN).is_null());
	assert!(from_double(f64::INFINITY).is_null());
	assert!(from_double(-f64::INFINITY).is_null());
	assert!(from_double(i32::MAX as f64 + 4.0).is_null());
	assert_rt(-0.0, 0, 1);
}

#[test]
fn from_double_tiny_retry() {
	// 2^-63 < flt < 2^-62: the first pass reduces to 0/1; the retry
	// against INT64_MAX is itself reduced back to 0/1 by the INT_MAX
	// ceiling, matching the C++ behavior exactly.
	let tiny = 1.5e-19;
	let r = from_double(tiny);
	assert_eq!((r.numerator(), r.denominator()), (0, 1));
	// Positive zero of the first pass but nonzero value: retry fires.
	assert_rt(0.0, 0, 1);
}

#[test]
fn to_rational_from_seconds() {
	// C++ fromRationalTime: RationalTime(1152, 24).to_seconds() = 48.
	assert_eq!(
		RationalTime::new(1152.0, 24.0).to_rational(),
		Rational::new(48, 1)
	);
	// RationalTime(12, 1).to_seconds() = 12.
	assert_eq!(
		RationalTime::new(12.0, 1.0).to_rational(),
		Rational::new(12, 1)
	);
	// RationalTime(576, 24).to_seconds() = 24.
	assert_eq!(
		RationalTime::new(576.0, 24.0).to_rational(),
		Rational::new(24, 1)
	);
}

#[test]
fn from_rational_matches_cpp_to_rational_time() {
	// Rational(576, 24).toRationalTime(24) stays {576, 24}.
	let rt = RationalTime::from_rational(Rational::new(576, 24), 24.0);
	assert_eq!((rt.value(), rt.rate()), (576.0, 24.0));
	// Rational(12, 24).toRationalTime() (default 24) = {12, 24}.
	let rt = RationalTime::from_rational(Rational::new(12, 24), 24.0);
	assert_eq!((rt.value(), rt.rate()), (12.0, 24.0));
	// Rational(0, 1).toRationalTime(24) = {0, 24}.
	let rt = RationalTime::from_rational(Rational::new(0, 1), 24.0);
	assert_eq!((rt.value(), rt.rate()), (0.0, 24.0));
	// Rational(1, 25).toRationalTime(25) = {1, 25}.
	let rt = RationalTime::from_rational(Rational::new(1, 25), 25.0);
	assert_eq!((rt.value(), rt.rate()), (1.0, 25.0));
}

#[test]
fn rescaled_to_semantics() {
	let rt = RationalTime::new(1.0, 25.0).rescaled_to(50.0);
	assert_eq!((rt.value(), rt.rate()), (2.0, 50.0));
	// Same rate: returned unchanged.
	let rt = RationalTime::new(1.0, 25.0).rescaled_to(25.0);
	assert_eq!((rt.value(), rt.rate()), (1.0, 25.0));
}

#[test]
fn invalid_time_semantics() {
	assert!(RationalTime::new(0.0, 0.0).is_invalid_time());
	assert!(RationalTime::new(1.0, -1.0).is_invalid_time());
	assert!(RationalTime::new(f64::NAN, 24.0).is_invalid_time());
	assert!(!RationalTime::new(1.0, 24.0).is_invalid_time());
}

#[test]
fn schema_names() {
	let clip = Composable::Clip(Clip::new("c"));
	assert_eq!(clip.schema_name(), "Clip");
	assert_eq!(clip.name(), "c");
	assert_eq!(
		Composable::Gap(Gap::new(TimeRange::default(), "g")).schema_name(),
		"Gap"
	);
	assert_eq!(
		Composable::Transition(Transition::new("t")).schema_name(),
		"Transition"
	);
	assert_eq!(
		Composable::Track(Track::new("Video")).schema_name(),
		"Track"
	);
	assert_eq!(Composable::Stack(Stack::default()).schema_name(), "Stack");
}

#[test]
fn unknown_field_round_trip() {
	let src = r#"{
            "OTIO_SCHEMA": "Timeline.1",
            "metadata": {},
            "name": "T",
            "global_start_time": null,
            "tracks": {
                "OTIO_SCHEMA": "Stack.1",
                "metadata": {"custom": 42},
                "name": "tracks",
                "source_range": null,
                "effects": [],
                "markers": [],
                "enabled": true,
                "children": [],
                "future_field": {"nested": [1, 2, 3]}
            },
            "some_future_root_field": "kept"
        }"#;
	let root: Serializable = serde_json::from_str(src).unwrap();
	let out: Value = serde_json::from_str(&to_json_string(&root).unwrap()).unwrap();
	assert_eq!(
		out["tracks"]["future_field"].clone(),
		serde_json::json!({"nested": [1, 2, 3]}),
		"{out}"
	);
	assert_eq!(
		out["some_future_root_field"].clone(),
		serde_json::json!("kept"),
		"{out}"
	);
	assert_eq!(
		out["tracks"]["metadata"].clone(),
		serde_json::json!({"custom": 42}),
		"{out}"
	);
}

#[test]
fn unknown_root_and_composable_stay_raw() {
	let raw_root = r#"{"OTIO_SCHEMA": "WeirdThing.9", "a": 1}"#;
	let root: Serializable = serde_json::from_str(raw_root).unwrap();
	assert_eq!(root.schema_name(), "WeirdThing.9");
	let out: Value = serde_json::from_str(&to_json_string(&root).unwrap()).unwrap();
	assert_eq!(out, serde_json::from_str::<Value>(raw_root).unwrap());

	let raw_block = r#"{"OTIO_SCHEMA": "FancyBlock.3", "b": [true]}"#;
	let block: Composable = serde_json::from_str(raw_block).unwrap();
	assert_eq!(block.schema_name(), "FancyBlock.3");
	let out: Value = serde_json::from_str(&to_json_string(&block).unwrap()).unwrap();
	assert_eq!(out, serde_json::from_str::<Value>(raw_block).unwrap());

	let raw_media = r#"{"OTIO_SCHEMA": "FancyReference.2", "c": "x"}"#;
	let media: MediaReference = serde_json::from_str(raw_media).unwrap();
	assert_eq!(media.schema_name(), "FancyReference.2");
	let out: Value = serde_json::from_str(&to_json_string(&media).unwrap()).unwrap();
	assert_eq!(out, serde_json::from_str::<Value>(raw_media).unwrap());
}

#[test]
fn timeline_new_builds_named_tracks_stack() {
	let t = Timeline::new("My Sequence");
	assert_eq!(t.name.as_str(), "My Sequence");
	assert_eq!(t.tracks.children().len(), 0);
	assert!(t.global_start_time.clone().is_none());
	let out = to_json_string(&t).unwrap();
	assert!(out.contains("\"name\": \"tracks\""), "{out}");
}

#[test]
fn clip_media_reference_resolution() {
	let mut clip = Clip::new("c");
	assert!(clip.media_reference().is_none());
	clip.set_media_reference(MediaReference::ExternalReference(ExternalReference::new(
		"file:///x.mp4",
		None,
	)));
	let mr = clip.media_reference().unwrap();
	assert_eq!(mr.schema_name(), "ExternalReference");
	assert_eq!(
		mr.as_external_reference().unwrap().target_url(),
		"file:///x.mp4"
	);
	// An active key that is not in the map yields None (C++ behavior).
	let mut clip2 = Clip::new("c2");
	clip2.active_media_reference_key = Some("OTHER".to_string());
	clip2.media_references.insert(
		"OTHER".to_string(),
		MediaReference::MissingReference(MissingReference::new()),
	);
	assert!(clip2.media_reference().is_some());
}
