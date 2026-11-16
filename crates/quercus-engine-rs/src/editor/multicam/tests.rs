use crate::edit::EditCommand;
use crate::editor::{EditorSession, TrackKind};
use crate::node::NodeId;
use crate::test_support;
use crate::types::Rational;
use std::env;
use std::fs;
use std::process;

#[test]
fn multicam_enable_switch_split_afv_and_undo_roundtrip() {
	let _guard = crate::node::UNDO_SERIAL
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let session = EditorSession::new().expect("session");
	let angles = session.create_sequence("Angles").expect("angles");
	for kind in [TrackKind::Video, TrackKind::Audio] {
		session
			.execute(EditCommand::AddTrack { kind })
			.expect("second angle track");
	}
	let media_a = test_support::make_test_clip_with_audio("multicam_a");
	let media_b = test_support::make_test_clip_with_audio("multicam_b");
	let footage_a = session.project().import_footage(&media_a).expect("angle A");
	let footage_b = session.project().import_footage(&media_b).expect("angle B");
	for (track_index, footage) in [footage_a, footage_b].into_iter().enumerate() {
		for kind in [TrackKind::Video, TrackKind::Audio] {
			session
				.execute(EditCommand::PlaceFootageClip {
					footage,
					kind,
					track_index,
					in_ts: 0,
					out_ts: 25,
					media_in_ts: 0,
				})
				.expect("place angle");
		}
	}

	let host = session.create_sequence("Host").expect("host");
	let host_video = session
		.execute(EditCommand::PlaceNestedSequenceClip {
			source: angles,
			track_index: 0,
			in_ts: 0,
			out_ts: 25,
		})
		.expect("host video")
		.primary
		.expect("host video id");
	let host_audio = session
		.execute(EditCommand::PlaceFootageClip {
			footage: footage_a,
			kind: TrackKind::Audio,
			track_index: 0,
			in_ts: 0,
			out_ts: 25,
			media_in_ts: 0,
		})
		.expect("host audio")
		.primary
		.expect("host audio id");
	session
		.execute(EditCommand::SetClipsLinked {
			clips: vec![host_video, host_audio],
			linked: true,
		})
		.expect("link host audio");
	session
		.execute(EditCommand::SetMulticamEnabled {
			clips: vec![host_video],
			enabled: true,
		})
		.expect("enable multicam");
	let state = session.multicam_state(host_video).expect("multicam state");
	assert_eq!(state.sequence, angles);
	assert_eq!(session.multicam_sequence(state.multicam), Some(angles));
	assert!(session.multicam_source_track(state.multicam, 1).is_some());
	assert!(session.multicam_source_track(state.multicam, 99).is_none());
	assert!(state.source_count >= 2);
	assert_eq!(state.current_source, 0);
	assert_eq!(
		session.multicam_connected_sequence(host_video),
		Some(angles)
	);
	assert!(session.multicam_is_clip(host_video));
	assert_eq!(
		session.multicam_clip_at(host, Rational::new(0, 1)),
		Some(host_video)
	);
	assert_eq!(
		session.clip_media_filename(host_audio).as_deref(),
		Some(media_a.as_str())
	);

	session
		.execute(EditCommand::SwitchMulticam {
			clip: host_video,
			source: 1,
			split_clip: false,
			playhead: Rational::new(0, 1),
		})
		.expect("switch to angle B");
	assert_eq!(
		session.multicam_state(host_video).unwrap().current_source,
		1
	);
	assert_eq!(
		session.clip_media_filename(host_audio).as_deref(),
		Some(media_b.as_str())
	);
	assert!(crate::node::undo());
	assert_eq!(
		session.multicam_state(host_video).unwrap().current_source,
		0
	);
	assert_eq!(
		session.clip_media_filename(host_audio).as_deref(),
		Some(media_a.as_str())
	);
	assert!(crate::node::redo());
	assert_eq!(
		session.multicam_state(host_video).unwrap().current_source,
		1
	);
	assert_eq!(
		session.clip_media_filename(host_audio).as_deref(),
		Some(media_b.as_str())
	);

	session
		.execute(EditCommand::SwitchMulticam {
			clip: host_video,
			source: 0,
			split_clip: true,
			playhead: Rational::new(1, 2),
		})
		.expect("split switch to angle A");
	let host_snapshot = session.snapshot();
	let host_clips: Vec<_> = host_snapshot
		.sequences
		.iter()
		.find(|sequence| sequence.id == host)
		.expect("host snapshot")
		.tracks
		.iter()
		.flat_map(|track| track.clips.iter())
		.collect();
	assert!(host_clips.len() >= 3, "split creates a new video block");
	assert!(host_clips.iter().any(|clip| {
		session
			.multicam_state(clip.id)
			.is_some_and(|state| state.current_source == 0)
	}));
	assert!(crate::node::undo());
	assert_eq!(
		session.multicam_state(host_video).unwrap().current_source,
		1
	);

	session
		.execute(EditCommand::SetMulticamEnabled {
			clips: vec![host_video],
			enabled: false,
		})
		.expect("disable multicam");
	assert!(session.multicam_state(host_video).is_none());
	assert!(crate::node::undo());
	assert!(session.multicam_state(host_video).is_some());

	let _ = fs::remove_file(media_a);
	let _ = fs::remove_file(media_b);
}

#[test]
fn multicam_wizard_builds_offsets_afv_and_survives_reload() {
	let _guard = crate::node::UNDO_SERIAL
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let session = EditorSession::new().expect("session");
	let media_a = test_support::make_test_clip_with_audio("wizard_a");
	let media_b = test_support::make_test_clip_with_audio("wizard_b");
	let angle_a = session.project().import_footage(&media_a).expect("angle A");
	let angle_b = session.project().import_footage(&media_b).expect("angle B");
	let host = session.create_sequence("Host").expect("host");
	assert!(
		session
			.execute(EditCommand::CreateMulticamSequence {
				host_sequence: host,
				angles: vec![angle_a],
				offsets: vec![0.0, 1.0],
				name: "Bad".into(),
			})
			.is_err()
	);
	let created = session
		.execute(EditCommand::CreateMulticamSequence {
			host_sequence: host,
			angles: vec![angle_a, angle_b],
			offsets: vec![0.0, 0.5],
			name: "Wizard".into(),
		})
		.expect("create wizard");
	let source = created.primary.expect("source sequence");
	assert_eq!(created.ids.len(), 1, "host clip is returned in ids");
	assert_eq!(session.current_sequence(), Some(host));
	let host_clip = session
		.multicam_clip_at(host, Rational::new(0, 1))
		.expect("host multicam clip");
	assert_eq!(created.ids[0], host_clip);
	let state = session.multicam_state(host_clip).expect("wizard state");
	assert_eq!(state.sequence, source);
	assert_eq!(state.source_count, 2);
	let snapshot = session.snapshot();
	let source_snapshot = snapshot
		.sequences
		.iter()
		.find(|sequence| sequence.id == source)
		.expect("source snapshot");
	let offset_clip = source_snapshot
		.tracks
		.iter()
		.filter(|track| track.kind == Some(TrackKind::Video))
		.flat_map(|track| track.clips.iter())
		.find(|clip| clip.in_time == Rational::new(1001, 2000))
		.expect("angle B offset");
	assert_eq!(offset_clip.out_time, Rational::new(3003, 2000));
	let host_audio = snapshot
		.sequences
		.iter()
		.find(|sequence| sequence.id == host)
		.expect("host snapshot")
		.tracks
		.iter()
		.find(|track| track.kind == Some(TrackKind::Audio))
		.and_then(|track| track.clips.first())
		.expect("host AFV audio")
		.id;
	assert_eq!(
		session.clip_media_filename(host_audio).as_deref(),
		Some(media_a.as_str())
	);

	let path = env::temp_dir()
		.join(format!("quercus_sdk_wizard_{}.ove", process::id()))
		.to_string_lossy()
		.into_owned();
	session.save(&path).expect("save wizard");
	let opened = EditorSession::open(&path).expect("reload wizard");
	let reloaded_clip = opened
		.multicam_clip_at(host, Rational::new(0, 1))
		.expect("reloaded host clip");
	let reloaded = opened
		.multicam_state(reloaded_clip)
		.expect("reloaded state");
	assert_eq!(reloaded.sequence, source);
	assert_eq!(reloaded.source_count, 2);
	assert_eq!(
		opened.clip_media_filename(host_audio).as_deref(),
		Some(media_a.as_str())
	);

	let _ = fs::remove_file(path);
	let _ = fs::remove_file(media_a);
	let _ = fs::remove_file(media_b);
}

#[test]
fn multicam_grid_matches_square_growth() {
	let expected = [
		(1, 1),
		(1, 2),
		(2, 2),
		(2, 2),
		(2, 3),
		(2, 3),
		(3, 3),
		(3, 3),
		(3, 3),
	];
	for (index, (rows, cols)) in expected.into_iter().enumerate() {
		let sources = index as i32 + 1;
		assert_eq!(EditorSession::multicam_grid(sources), Some((rows, cols)));
		assert_eq!(super::rows_and_columns(sources), (rows, cols));
	}
	assert_eq!(EditorSession::multicam_grid(0), None);
	assert_eq!(EditorSession::multicam_grid(-3), None);
}

#[test]
fn multicam_source_at_hit_tests_grid() {
	assert_eq!(EditorSession::multicam_source_at(1, 0.0, 0.0), Some(0));
	assert_eq!(EditorSession::multicam_source_at(1, 0.999, 0.999), Some(0));
	// 3 sources -> 2x2; index 2 is the bottom-left cell.
	assert_eq!(EditorSession::multicam_source_at(3, 0.25, 0.75), Some(2));
	// 5 sources -> 2 rows x 3 cols; index 4 is the last filled cell.
	assert_eq!(EditorSession::multicam_source_at(5, 0.5, 0.6), Some(4));
	// The trailing cell of that non-square grid is empty.
	assert_eq!(EditorSession::multicam_source_at(5, 0.9, 0.6), None);
	// Outside the normalized range (1.0 maps past the last row/column).
	assert_eq!(EditorSession::multicam_source_at(2, 1.0, 0.0), None);
	assert_eq!(EditorSession::multicam_source_at(2, 2.0, 0.0), None);
	assert_eq!(EditorSession::multicam_source_at(2, 0.0, 1.0), None);
	assert_eq!(EditorSession::multicam_source_at(2, -0.1, 0.0), None);
	// Invalid counts.
	assert_eq!(EditorSession::multicam_source_at(0, 0.5, 0.5), None);
	assert_eq!(EditorSession::multicam_source_at(-1, 0.5, 0.5), None);
}

#[test]
fn multicam_sources_expose_track_names_and_current() {
	let _guard = crate::node::UNDO_SERIAL
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let session = EditorSession::new().expect("session");
	let media_a = test_support::make_test_clip_with_audio("sources_a");
	let media_b = test_support::make_test_clip_with_audio("sources_b");
	let angle_a = session.project().import_footage(&media_a).expect("angle A");
	let angle_b = session.project().import_footage(&media_b).expect("angle B");
	let host = session.create_sequence("Host").expect("host");
	let source = session
		.execute(EditCommand::CreateMulticamSequence {
			host_sequence: host,
			angles: vec![angle_a, angle_b],
			offsets: vec![0.0, 0.5],
			name: "Sources".into(),
		})
		.expect("create multicam")
		.primary
		.expect("source sequence");
	let host_clip = session
		.multicam_clip_at(host, Rational::new(0, 1))
		.expect("host multicam clip");
	let state = session.multicam_state(host_clip).expect("multicam state");
	assert_eq!(state.sequence, source);
	assert_eq!(
		session.multicam_source_count(state.multicam),
		Some(state.source_count)
	);
	assert_eq!(
		session.multicam_current_source(state.multicam),
		Some(state.current_source)
	);

	let sources = session.multicam_sources(state.multicam);
	assert_eq!(sources.len() as i32, state.source_count);
	assert!(sources.len() >= 2);
	assert!(sources.iter().all(|source| !source.name.is_empty()));
	assert_eq!(sources[0].name, "Video Track 0");
	assert_eq!(sources[1].name, "Video Track 1");
	assert_eq!(sources.iter().filter(|source| source.current).count(), 1);
	for (position, source) in sources.iter().enumerate() {
		assert_eq!(source.index, position as i32);
		assert_eq!(source.current, source.index == state.current_source);
		assert_eq!(
			session.multicam_source_track(state.multicam, source.index),
			Some(source.track)
		);
	}

	assert!(session.multicam_sources(NodeId::INVALID).is_empty());
	assert_eq!(session.multicam_source_count(NodeId::INVALID), None);

	let _ = fs::remove_file(media_a);
	let _ = fs::remove_file(media_b);
}
