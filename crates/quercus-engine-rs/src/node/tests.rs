use super::*;

/// Layout assertions shared with `include/quercus_engine.h`.
#[test]
fn abi_mirror_layout_matches_c_header() {
	assert_eq!(std::mem::size_of::<NodeValue>(), 56);
	assert_eq!(
		std::mem::size_of::<crate::editor::StorageProjectHandle>(),
		32
	);
}

/// History navigation/inspection and the observer hook (the undo
/// stack is process-global; serialize).

#[test]
fn undo_history_rows_and_observer() {
	let _guard = UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
	static SEEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
	add_undo_observer(|| {
		SEEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
	});

	let project = Project::new();
	let base = undo_count();
	project
		.create_folder(project.root(), "Observed")
		.expect("folder");
	assert_eq!(undo_count(), base + 1);
	assert!(SEEN.load(std::sync::atomic::Ordering::SeqCst) > 0);

	assert!(undo_command_name(base).is_some());
	assert_eq!(undo_command_done(base), Some(true));
	assert!(undo_jump(base));
	assert_eq!(undo_command_done(base), Some(false));
	assert!(undo_jump(base + 1));
	assert_eq!(undo_command_done(base), Some(true));

	assert!(undo_clear());
	assert_eq!(undo_count(), 1, "clear leaves the fresh empty row");
}

#[test]
fn node_label_and_sequence_params() {
	let _guard = UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
	let project = Project::new();
	let folder = project
		.create_folder(project.root(), "Labeled")
		.expect("folder");
	assert_eq!(project.node_label(folder), "Labeled");
	assert!(project.node_set_label(folder, "Renamed"));
	assert_eq!(project.node_label(folder), "Renamed");
	assert!(!project.node_set_label(NodeId::INVALID, "x"));

	let seq = project.create_sequence("Seq").expect("sequence");
	// A fresh sequence carries the default 29.97 fps video params.
	assert_eq!(project.sequence_frame_rate(seq), Rational::new(30000, 1001));
	assert!(project.sequence_geometry(seq).is_some());
	assert!(project.sequence_set_video_params(seq, 1920, 1080, 24, 1));
	assert_eq!(project.sequence_frame_rate(seq), Rational::new(24, 1));
	assert_eq!(project.sequence_geometry(seq), Some((1920, 1080)));
	assert!(!project.sequence_set_video_params(NodeId::INVALID, 1, 1, 1, 1));
	assert!(undo_clear());
}

#[test]
fn project_basics_and_node_ids() {
	let project = Project::new();
	assert!(!project.modified());
	assert!(project.filename().is_empty());

	let root = project.root();
	assert!(root.is_valid());
	assert!(!NodeId::INVALID.is_valid());
	assert_eq!(project.node_kind(root), Some(NodeKind::Folder));
	assert_eq!(project.node_kind(NodeId::INVALID), None);
	assert!(
		project
			.node_type_id(root)
			.is_some_and(|id| id.ends_with("folder"))
	);
	assert_eq!(project.folder_child_count(root), 0);

	// Clone shares the project: same root.
	let clone = project.clone();
	assert_eq!(clone.root(), root);
}

#[test]
fn active_project_slot() {
	set_active_project(None);
	assert!(active_project().is_none());

	let project = Project::new();
	set_active_project(Some(&project));
	let active = active_project().expect("active");
	assert_eq!(active.root(), project.root());

	set_active_project(None);
	assert!(active_project().is_none());
}

#[test]
fn undo_read_only() {
	// Fresh process-wide stack state is whatever the suite left;
	// just verify the calls work.
	let _ = undo_can_undo();
	let _ = undo_can_redo();
	assert!(undo_count() >= 0);
}

#[test]
fn unreadable_nodes_fail_cleanly() {
	let project = Project::new();
	let root = project.root();
	// The root folder has no "file_in" input; and no node reads work
	// on an invalid id.
	assert!(project.standard_value(root, "file_in", -1).is_none());
	assert!(
		project
			.standard_value(NodeId::INVALID, "file_in", -1)
			.is_none()
	);
	assert!(
		project
			.value_at_time(root, "file_in", -1, Rational::new(0, 1))
			.is_none()
	);
	assert_eq!(project.footage_filename(root), None, "root is a folder");
	assert_eq!(project.track_block_count(root), 0);
	assert_eq!(project.sequence_track_list_at(root, 0), NodeId::INVALID);
}
