use super::*;

/// The C entry points must reject null/invalid handles before touching the
/// project; these are the defensive branches every UI caller relies on.
#[test]
fn null_and_invalid_inputs_are_rejected() {
	let session = crate::editor::quercus_editor_new();
	assert!(!session.is_null());
	let mut state = QuercusEditorMulticamState::default();
	unsafe {
		assert!(!quercus_editor_multicam_state(
			std::ptr::null(),
			NodeId::INVALID.identity(),
			&mut state
		));
		assert!(!quercus_editor_multicam_state(
			session,
			u64::MAX,
			&mut state
		));
		assert!(!quercus_editor_multicam_state(
			session,
			u64::MAX,
			std::ptr::null_mut()
		));
		assert_eq!(
			quercus_editor_multicam_connected_sequence(std::ptr::null(), 0),
			QUERCUS_NODE_ID_INVALID
		);
		assert_eq!(
			quercus_editor_multicam_connected_sequence(session, u64::MAX),
			QUERCUS_NODE_ID_INVALID
		);
		assert!(!quercus_editor_multicam_is_clip(std::ptr::null(), 0));
		assert!(!quercus_editor_multicam_is_clip(session, u64::MAX));
		assert_eq!(
			quercus_editor_multicam_clip_at(std::ptr::null(), 0, QuercusRational::default()),
			QUERCUS_NODE_ID_INVALID
		);
		assert_eq!(
			quercus_editor_multicam_clip_at(session, u64::MAX, QuercusRational::default()),
			QUERCUS_NODE_ID_INVALID
		);
		assert_eq!(
			quercus_editor_multicam_sequence(std::ptr::null(), 0),
			QUERCUS_NODE_ID_INVALID
		);
		assert_eq!(
			quercus_editor_multicam_sequence(session, u64::MAX),
			QUERCUS_NODE_ID_INVALID
		);
		assert_eq!(
			quercus_editor_multicam_source_track(std::ptr::null(), 0, 0),
			QUERCUS_NODE_ID_INVALID
		);
		assert_eq!(
			quercus_editor_multicam_source_track(session, u64::MAX, 0),
			QUERCUS_NODE_ID_INVALID
		);
		assert_eq!(
			quercus_editor_multicam_source_count(std::ptr::null(), 0),
			-1
		);
		assert_eq!(quercus_editor_multicam_source_count(session, u64::MAX), -1);
		assert_eq!(
			quercus_editor_multicam_current_source(std::ptr::null(), 0),
			-1
		);
		assert_eq!(
			quercus_editor_multicam_current_source(session, u64::MAX),
			-1
		);
		assert_eq!(
			quercus_editor_multicam_source_name(std::ptr::null(), 0, 0, std::ptr::null_mut(), 0),
			-1
		);
		assert_eq!(
			quercus_editor_multicam_source_name(session, u64::MAX, -1, std::ptr::null_mut(), 0),
			-1
		);
		assert_eq!(
			quercus_editor_multicam_source_name(session, u64::MAX, 0, std::ptr::null_mut(), 0),
			-1
		);
		let mut rows = 0;
		let mut cols = 0;
		assert!(!quercus_editor_multicam_grid(0, &mut rows, &mut cols));
		assert!(!quercus_editor_multicam_grid(
			3,
			std::ptr::null_mut(),
			&mut cols
		));
		assert!(!quercus_editor_multicam_grid(
			3,
			&mut rows,
			std::ptr::null_mut()
		));
		assert!(!quercus_editor_multicam_grid(
			3,
			std::ptr::null_mut(),
			std::ptr::null_mut()
		));
		assert_eq!(quercus_editor_multicam_source_at(0, 0.0, 0.0), -1);
		assert_eq!(quercus_editor_multicam_source_at(3, -0.1, 0.0), -1);
		assert_eq!(quercus_editor_multicam_source_at(3, 0.0, 1.1), -1);
		assert_eq!(quercus_editor_multicam_source_at(3, 0.99, 0.99), -1);
		assert_eq!(quercus_editor_multicam_source_at(3, f64::NAN, 0.0), -1);
		crate::editor::quercus_editor_release(session);
	}
}
/// Exercise the valid-session early returns: a non-multicam node, a
/// multicam node without a sequence, and a valid id absent from the graph.
#[test]
fn valid_session_invalid_graph_paths() {
	let session = crate::editor::quercus_editor_new();
	assert!(!session.is_null());
	let project = unsafe { &*session }.project();
	let (multicam, plain_clip, missing) = {
		let mut guard = project.lock().unwrap();
		let (core, behavior) = quercus_node::nodes::multicamnode::create();
		let multicam = guard.graph.add_node(core, behavior);
		let (core, behavior) = quercus_node::block::clip_create();
		let plain_clip = guard.graph.add_node(core, behavior);
		let missing = NodeId::from_identity(999_999).unwrap();
		(multicam, plain_clip, missing)
	};
	let mut state = QuercusEditorMulticamState::default();
	unsafe {
		// Valid session + non-multicam clip: multicam_state returns false.
		assert!(!quercus_editor_multicam_state(
			session,
			plain_clip.identity(),
			&mut state
		));
		// Valid session + valid clip but null output: false before touching it.
		assert!(!quercus_editor_multicam_state(
			session,
			plain_clip.identity(),
			std::ptr::null_mut()
		));
		// Valid multicam without sequence: count 0, current 0, name -1.
		assert_eq!(
			quercus_editor_multicam_source_count(session, multicam.identity()),
			0
		);
		assert_eq!(
			quercus_editor_multicam_current_source(session, multicam.identity()),
			0
		);
		assert_eq!(
			quercus_editor_multicam_source_name(
				session,
				multicam.identity(),
				0,
				std::ptr::null_mut(),
				0
			),
			-1
		);
		// Valid identity that is no longer in the graph: early -1 path.
		assert_eq!(
			quercus_editor_multicam_source_count(session, missing.identity()),
			-1
		);
		assert_eq!(
			quercus_editor_multicam_current_source(session, missing.identity()),
			-1
		);
		assert_eq!(
			quercus_editor_multicam_source_name(
				session,
				missing.identity(),
				0,
				std::ptr::null_mut(),
				0
			),
			-1
		);
		// 1x2 grid: a point below the first row is outside the grid.
		assert_eq!(quercus_editor_multicam_source_at(2, 0.25, 0.75), -1);
		// Grid outputs for a valid count.
		let mut rows = 0;
		let mut cols = 0;
		assert!(quercus_editor_multicam_grid(2, &mut rows, &mut cols));
		assert_eq!((rows, cols), (1, 2));
		crate::editor::quercus_editor_release(session);
	}
}
