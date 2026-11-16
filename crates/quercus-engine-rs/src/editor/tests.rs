use super::*;
use crate::node::NodeKind;
use crate::state::Config;
use crate::types::Rational;

#[test]
fn file_round_trip_preserves_the_owned_snapshot() {
	let _guard = crate::node::UNDO_SERIAL
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let session = EditorSession::new().expect("new session");
	let folder = session
		.project()
		.create_folder(session.project().root(), "Nested Bin")
		.expect("nested folder");
	let media = crate::test_support::make_test_clip("snapshot");
	let footage = session.project().import_footage(&media).expect("footage");
	let sequence = session.create_sequence("SDK Sequence").expect("sequence");
	let project = session.project();
	let list = project.sequence_track_list_at(sequence, 0);
	let track = project.tracklist_track_at(list, 0);
	let clip = project.create_clip().expect("clip");
	assert!(project.track_append_block(track, clip));
	assert!(project.block_set_length(clip, Rational::new(2, 1)));

	let enabled = session.snapshot();
	assert!(enabled.sequences[0].tracks[0].clips[0].enabled);
	assert!(crate::timeline::block_set_enabled(&project, clip, false));
	let disabled = session.snapshot();
	assert!(!disabled.sequences[0].tracks[0].clips[0].enabled);
	assert!(disabled.revision > enabled.revision);
	assert!(crate::node::undo());
	assert!(session.snapshot().sequences[0].tracks[0].clips[0].enabled);
	assert!(crate::node::redo());
	assert!(!session.snapshot().sequences[0].tracks[0].clips[0].enabled);

	let before = session.snapshot();
	assert_eq!(before.current_sequence, Some(sequence));
	assert_eq!(before.sequences.len(), 1);
	assert_eq!(before.sequences[0].tracks[0].clips.len(), 1);
	assert_eq!(before.footages.len(), 1);
	assert_eq!(before.footages[0].id, footage);
	assert_eq!(before.footages[0].filename.as_deref(), Some(media.as_str()));
	assert!(before.footages[0].video_streams >= 1);
	assert_eq!(before.footages[0].video.map(|p| p.width), Some(64));
	assert!(before.footages[0].duration > Rational::new(0, 1));
	assert!(before.root.children.iter().any(|entry| entry.id == folder));
	assert!(before.markers.is_empty());
	assert!(before.workarea.is_none());

	let path = std::env::temp_dir()
		.join(format!("quercussdk_editor_{}.ove", std::process::id()))
		.to_string_lossy()
		.into_owned();
	session.save(&path).expect("save");
	let opened = EditorSession::open(&path).expect("open");
	let after = opened.snapshot();
	assert_eq!(after.filename, path);
	assert!(!after.modified);
	assert_eq!(after.current_sequence, Some(sequence));
	assert_eq!(after.sequences, before.sequences);
	assert_eq!(after.footages, before.footages);
	assert_eq!(after.root.children, before.root.children);
	assert_eq!(after.root.kind, Some(NodeKind::Folder));
	assert!(after.revision >= before.revision);
	let _ = std::fs::remove_file(path);
	let _ = std::fs::remove_file(media);
}

#[test]
fn project_settings_and_cache_round_trip() {
	let _guard = crate::node::UNDO_SERIAL
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let session = EditorSession::new().expect("new session");
	let project = session.project();
	assert_eq!(project.setting("missing"), None);
	assert!(!project.set_setting("", "rejected"));
	assert!(project.set_setting("ocioconfig", "/tmp/test.ocio"));
	assert_eq!(
		project.setting("ocioconfig").as_deref(),
		Some("/tmp/test.ocio")
	);
	assert!(project.set_cache_location(2, "/tmp/quercus-cache"));
	assert_eq!(project.cache_location(), (2, "/tmp/quercus-cache".into()));
	assert!(project.set_cache_location(0, "/ignored"));
	assert_eq!(project.cache_location(), (0, String::new()));
	assert!(project.set_cache_location(-10, "/ignored"));
	assert_eq!(project.cache_location().0, 0);
	assert!(project.set_cache_location(99, "  /tmp/custom  "));
	assert_eq!(project.cache_location(), (2, "/tmp/custom".into()));
}

#[test]
fn invalid_inputs_and_shared_close_semantics() {
	let _guard = crate::node::UNDO_SERIAL
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	Config::global()
		.set("Storage/Backend", "off")
		.expect("disable library");
	assert!(EditorSession::open("not-a-storage-uri").is_err());
	assert!(EditorSession::open_project_from_library("missing").is_err());

	let session = EditorSession::new().expect("new session");
	let first = session.create_sequence("First").expect("first");
	let second = session.create_sequence("Second").expect("second");
	assert_eq!(session.current_sequence(), Some(second));
	assert!(session.set_current_sequence(first));
	assert!(!session.set_current_sequence(NodeId::INVALID));
	assert_eq!(session.current_sequence(), Some(first));
	assert_eq!(session.snapshot().sequences.len(), 2);

	let cloned = session.clone();
	cloned.close();
	assert!(!cloned.storage_bound());
	assert!(!session.storage_bound(), "clones share one session");
	let project = session.project();
	drop(cloned);
	drop(session);
	assert!(
		project.root().is_valid(),
		"the project handle owns its own reference"
	);
}
