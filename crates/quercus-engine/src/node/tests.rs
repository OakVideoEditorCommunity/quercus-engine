use super::*;

fn cstr_arg(s: &str) -> (*const u8, usize) {
	(s.as_ptr(), s.len())
}

/// A project with one footage node in the root folder.
fn project_with_footage() -> *mut ProjectHandle {
	let project = quercus_project_new();
	assert!(!project.is_null());
	// Build a footage node through quercus-node directly (the engine's
	// read API is what's under test).
	{
		let inner = unsafe { &*project }.inner.clone();
		let mut g = inner.lock().unwrap();
		let (core, mut behavior) = quercus_node::footage::FootageBehavior::create();
		{
			let b = behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<FootageBehavior>()
				.unwrap();
			b.filename = "/tmp/quercus-node-test.mp4".to_string();
		}
		let id = g.graph.add_node(core, behavior);
		let root = g.root;
		g.graph
			.get_mut(root)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<FolderBehavior>()
			.unwrap()
			.add_child(id);
	}
	project
}

#[test]
fn id_tokens_and_project_basics() {
	assert_eq!(quercus_node_id_invalid(), u32::MAX as u64);
	assert!(!quercus_node_id_is_valid(quercus_node_id_invalid()));

	let project = quercus_project_new();
	unsafe {
		let root = quercus_project_root(project);
		assert!(
			quercus_node_id_is_valid(root),
			"initialized project has a root"
		);
		assert_eq!(quercus_node_kind(project, root), 1, "root is a folder");
		assert_eq!(quercus_folder_child_count(project, root), 0);

		let mut buf = [0u8; 128];
		let n = quercus_node_type_id(project, root, buf.as_mut_ptr(), buf.len());
		assert!(
			std::str::from_utf8(&buf[..n as usize])
				.unwrap()
				.ends_with("folder")
		);
		assert!(quercus_project_filename(project, buf.as_mut_ptr(), buf.len()) >= 0);
		assert!(!quercus_project_modified(project));

		quercus_project_release(project);
	}
}

/// Node labels and sequence introspection through the C ABI.
#[test]
fn node_label_and_sequence_params() {
	// Folder creation is undoable; serialize against the other
	// undo-stack tests.
	let _guard = UNDO_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
	unsafe {
		let project = quercus_project_new();
		let root = quercus_project_root(project);
		let (n, nl) = ("Labeled".as_ptr(), 7);
		let folder = quercus_folder_create(project, root, n, nl);
		assert!(quercus_node_id_is_valid(folder));

		let mut buf = [0u8; 64];
		let len = quercus_node_label(project, folder, buf.as_mut_ptr(), buf.len());
		assert_eq!(
			std::str::from_utf8(&buf[..len as usize]).unwrap(),
			"Labeled"
		);
		assert!(quercus_node_set_label(
			project,
			folder,
			"Renamed".as_ptr(),
			7
		));
		let len = quercus_node_label(project, folder, buf.as_mut_ptr(), buf.len());
		assert_eq!(
			std::str::from_utf8(&buf[..len as usize]).unwrap(),
			"Renamed"
		);
		assert_eq!(
			quercus_node_label(project, u64::MAX, buf.as_mut_ptr(), 64),
			-1
		);
		assert!(!quercus_node_set_label(project, u64::MAX, "x".as_ptr(), 1));

		let (s, sl) = ("Seq".as_ptr(), 3);
		let seq = quercus_sequence_create(project, s, sl);
		// A fresh sequence carries the default 29.97 fps video params.
		assert_eq!(
			quercus_sequence_frame_rate(project, seq),
			QuercusRational {
				num: 30000,
				den: 1001
			}
		);
		assert!(quercus_sequence_geometry(
			project,
			seq,
			std::ptr::null_mut(),
			std::ptr::null_mut()
		));
		assert!(quercus_sequence_set_video_params(
			project, seq, 1920, 1080, 24, 1
		));
		assert_eq!(
			quercus_sequence_frame_rate(project, seq),
			QuercusRational { num: 24, den: 1 }
		);
		let (mut w, mut h) = (0, 0);
		assert!(quercus_sequence_geometry(project, seq, &mut w, &mut h));
		assert_eq!((w, h), (1920, 1080));
		assert_eq!(
			quercus_sequence_length(project, seq),
			QuercusRational { num: 0, den: 1 }
		);
		assert!(!quercus_sequence_set_video_params(
			project,
			u64::MAX,
			1,
			1,
			1,
			1
		));

		quercus_project_release(project);
		assert!(quercus_undo::global::clear().is_ok());
	}
}

#[test]
fn active_project_slot() {
	unsafe {
		quercus_project_set_active(std::ptr::null());
		assert!(quercus_project_active().is_null());

		let project = quercus_project_new();
		quercus_project_set_active(project);
		let active = quercus_project_active();
		assert!(!active.is_null());
		// Same underlying project: the root token matches.
		assert_eq!(quercus_project_root(active), quercus_project_root(project));

		quercus_project_set_active(std::ptr::null());
		assert!(quercus_project_active().is_null());

		quercus_project_release(active);
		quercus_project_release(project);
	}
}

#[test]
fn footage_traversal() {
	let project = project_with_footage();
	unsafe {
		let root = quercus_project_root(project);
		assert_eq!(quercus_folder_child_count(project, root), 1);
		let child = quercus_folder_child_at(project, root, 0);
		assert!(quercus_node_id_is_valid(child));
		assert_eq!(quercus_node_kind(project, child), 2, "footage");

		let mut buf = [0u8; 128];
		let n = quercus_footage_filename(project, child, buf.as_mut_ptr(), buf.len());
		assert_eq!(
			std::str::from_utf8(&buf[..n as usize]).unwrap(),
			"/tmp/quercus-node-test.mp4"
		);

		// Unprobed: zero streams.
		assert_eq!(quercus_footage_total_stream_count(project, child), 0);
		assert!(!quercus_footage_video_params(
			project,
			child,
			0,
			std::ptr::null_mut()
		));

		quercus_project_release(project);
	}
}

#[test]
fn sequence_track_tracklist_traversal() {
	let project = quercus_project_new();
	{
		let inner = unsafe { &*project }.inner.clone();
		let mut g = inner.lock().unwrap();
		// sequence → tracklist(video) → track(video)
		let (seq_core, seq_b) = quercus_node::sequence::SequenceBehavior::create();
		let seq = g.graph.add_node(seq_core, seq_b);
		let (tl_core, tl_b) = quercus_node::track::TrackListBehavior::create();
		let tl = g.graph.add_node(tl_core, tl_b);
		let (tr_core, tr_b) = quercus_node::track::TrackBehavior::create();
		let tr = g.graph.add_node(tr_core, tr_b);

		let tl_b = g
			.graph
			.get_mut(tl)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TrackListBehavior>()
			.unwrap();
		tl_b.tracks.push(tr);
		let seq_b = g
			.graph
			.get_mut(seq)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<SequenceBehavior>()
			.unwrap();
		seq_b.track_lists.push(tl);
	}
	unsafe {
		let root = quercus_project_root(project);
		let _ = root;
		// find the sequence via graph scan (it is not in the bin)
		// — simpler: the traversal entry points are exercised directly.
		// (sequence id is not discoverable via the folder tree)
		let mut found_seq = 0u64;
		{
			let inner = &*project;
			let g = inner.inner.lock().unwrap();
			for id in g.graph.node_ids() {
				if g.graph.get(id).unwrap().behavior.type_id()
					== "org.olivevideoeditor.Olive.sequence"
				{
					found_seq = id.identity();
				}
			}
		}
		assert_eq!(quercus_sequence_track_list_count(project, found_seq), 1);
		let tl = quercus_sequence_track_list_at(project, found_seq, 0);
		assert_eq!(quercus_tracklist_track_count(project, tl), 1);
		let tr = quercus_tracklist_track_at(project, tl, 0);
		assert_eq!(quercus_track_block_count(project, tr), 0);
		quercus_project_release(project);
	}
}

#[test]
fn param_reads_and_undo_readonly() {
	let project = project_with_footage();
	unsafe {
		let root = quercus_project_root(project);
		let child = quercus_folder_child_at(project, root, 0);

		// footage has a "file_in" text input — not POD-representable,
		// so the read must fail cleanly (false), not crash.
		let mut out = quercus_node::value::QuercusNodeValue::none();
		let (i, il) = cstr_arg("file_in");
		assert!(!quercus_node_standard_value(
			project, child, i, il, -1, &mut out
		));
		// Unknown input id: false.
		let (i, il) = cstr_arg("no_such_input");
		assert!(!quercus_node_standard_value(
			project, child, i, il, -1, &mut out
		));
		// value_at_time on a static text input: also false (string).
		let (i, il) = cstr_arg("file_in");
		assert!(!quercus_node_value_at_time(
			project,
			child,
			i,
			il,
			-1,
			QuercusRational { num: 0, den: 1 },
			&mut out,
		));

		// Fresh process-wide stack state is whatever the suite left;
		// just verify the calls work.
		let _ = quercus_undo_can_undo();
		let _ = quercus_undo_can_redo();
		assert!(quercus_undo_count() >= 0);
		assert!(quercus_undo_index() >= 0 || quercus_undo_count() == 0);

		quercus_project_release(project);
	}
}

#[test]
fn null_safety_sweep() {
	unsafe {
		let mut buf = [0u8; 64];
		assert_eq!(quercus_node_kind(std::ptr::null(), 1), 0);
		assert_eq!(
			quercus_node_type_id(std::ptr::null(), 1, buf.as_mut_ptr(), 64),
			-1
		);
		assert_eq!(quercus_folder_child_count(std::ptr::null(), 1), 0);
		assert_eq!(
			quercus_folder_child_at(std::ptr::null(), 1, 0),
			QUERCUS_NODE_ID_INVALID
		);
		assert_eq!(
			quercus_footage_filename(std::ptr::null(), 1, buf.as_mut_ptr(), 64),
			-1
		);
		assert_eq!(quercus_footage_duration(std::ptr::null(), 1).den, 0);
		assert_eq!(quercus_sequence_track_list_count(std::ptr::null(), 1), 0);
		assert_eq!(quercus_tracklist_kind(std::ptr::null(), 1), -1);
		assert_eq!(quercus_track_length(std::ptr::null(), 1).num, 0);
		assert_eq!(quercus_block_in(std::ptr::null(), 1).den, 0);
		assert!(!quercus_project_modified(std::ptr::null()));
		assert_eq!(
			quercus_project_root(std::ptr::null()),
			QUERCUS_NODE_ID_INVALID
		);
		quercus_project_release(std::ptr::null());
	}
}
