//! Independent SDK-consumer workflow for P3 graph/effect/parameter validation.

#[cfg(test)]
mod tests {
	use crate::edit::EditCommand;
	use crate::editor::{EditorSession, KeyframeInterpolation, TrackKind};
	use crate::node::NodeValue;
	use crate::ofx;
	use crate::render::RenderBackend;
	use crate::test_support;
	use crate::types::Rational;
	use std::env;
	use std::fs;
	use std::process;

	fn float(value: f64) -> NodeValue {
		let mut raw = NodeValue {
			kind: 2,
			..NodeValue::default()
		};
		raw.f[0] = value;
		raw
	}

	#[test]
	fn graph_effect_keyframes_render_and_reload_without_internal_crates() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let Some(plugin_dir) = test_support::ofx_plugin_scan_dir() else {
			println!("SKIP: bundled CPU test plugin is unavailable");
			return;
		};
		assert!(ofx::scan_path(&plugin_dir.to_string_lossy()));
		ofx::register_plugin_nodes();
		let _ = crate::render::manager_init(RenderBackend::Threads);
		let session = EditorSession::new().expect("session");
		let sequence = session.create_sequence("Independent").expect("sequence");
		let media = test_support::make_test_clip("p3_workflow");
		let footage = session.project().import_footage(&media).expect("footage");
		let clip = session
			.execute(EditCommand::PlaceFootageClip {
				footage,
				kind: TrackKind::Video,
				track_index: 0,
				in_ts: 0,
				out_ts: 60,
				media_in_ts: 0,
			})
			.expect("place")
			.primary
			.expect("clip");
		let effect = session
			.execute(EditCommand::EffectInsert {
				host: clip,
				index: usize::MAX,
				type_id: "org.quercus.test-plugin".into(),
			})
			.expect("color correct")
			.primary
			.expect("effect");
		for (time, saturation) in [(0, 0.0), (1, 1.0)] {
			session
				.execute(EditCommand::UpsertNodeKeyframe {
					node: effect,
					input_id: "gain".into(),
					element: -1,
					time: Rational::new(time, 1),
					value: float(saturation),
					interpolation: KeyframeInterpolation::Linear,
					bezier_in: (0.0, 0.0),
					bezier_out: (0.0, 0.0),
				})
				.expect("saturation key");
		}
		let graph = session.graph_snapshot(sequence, Some(clip)).expect("graph");
		assert!(graph.nodes.iter().any(|node| node.id == effect));
		let parameters = session.parameters(effect).expect("parameters");
		assert_eq!(parameters.get("gain").unwrap().track(-1).unwrap().len(), 2);
		let at_zero = session
			.project()
			.value_at_time(effect, "gain", -1, Rational::new(0, 1))
			.expect("gain at zero");
		let at_one = session
			.project()
			.value_at_time(effect, "gain", -1, Rational::new(1, 1))
			.expect("gain at one");
		assert_eq!(at_zero.f[0], 0.0);
		assert_eq!(at_one.f[0], 1.0);
		session
			.execute(EditCommand::PushNodeButton {
				node: effect,
				input_id: "button".into(),
			})
			.expect("CPU plugin button");

		let rendered = crate::render::render_frame_f32(
			&session.project(),
			sequence,
			Rational::new(1, 1),
			(32, 16),
		)
		.expect("CPU effect frame");
		assert_eq!(rendered.len(), 32 * 16 * 4);
		assert!(
			rendered
				.chunks_exact(4)
				.all(|pixel| pixel == [0.5, 0.5, 0.5, 1.0])
		);

		let path = env::temp_dir()
			.join(format!("quercus_p3_workflow_{}.ove", process::id()))
			.to_string_lossy()
			.into_owned();
		session.save(&path).expect("save workflow");
		let opened = EditorSession::open(&path).expect("reload workflow");
		let reloaded = opened.parameters(effect).expect("reloaded parameters");
		assert_eq!(reloaded.get("gain").unwrap().track(-1).unwrap().len(), 2);
		let reopened = crate::render::render_frame_f32(
			&opened.project(),
			sequence,
			Rational::new(1, 1),
			(32, 16),
		)
		.expect("reloaded render");
		assert_eq!(reopened, rendered);
		let _ = fs::remove_file(path);
		let _ = fs::remove_file(media);
	}

	#[test]
	fn animated_gain_keyframes_render_interpolate_and_reload() {
		let _guard = crate::node::UNDO_SERIAL
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let Some(plugin_dir) = test_support::ofx_plugin_scan_dir() else {
			println!("SKIP: bundled CPU test plugin is unavailable");
			return;
		};
		assert!(ofx::scan_path(&plugin_dir.to_string_lossy()));
		ofx::register_plugin_nodes();
		let _ = crate::render::manager_init(RenderBackend::Threads);
		let session = EditorSession::new().expect("session");
		let sequence = session.create_sequence("AnimatedGain").expect("sequence");
		let media = test_support::make_test_clip("p3_animated_gain");
		let footage = session.project().import_footage(&media).expect("footage");
		let clip = session
			.execute(EditCommand::PlaceFootageClip {
				footage,
				kind: TrackKind::Video,
				track_index: 0,
				in_ts: 0,
				out_ts: 60,
				media_in_ts: 0,
			})
			.expect("place")
			.primary
			.expect("clip");
		let effect = session
			.execute(EditCommand::EffectInsert {
				host: clip,
				index: usize::MAX,
				type_id: "org.quercus.test-plugin".into(),
			})
			.expect("insert effect")
			.primary
			.expect("effect");

		for (time, gain) in [(0, 0.0), (1, 1.0)] {
			session
				.execute(EditCommand::UpsertNodeKeyframe {
					node: effect,
					input_id: "animatedGain".into(),
					element: -1,
					time: Rational::new(time, 1),
					value: float(gain),
					interpolation: KeyframeInterpolation::Linear,
					bezier_in: (0.0, 0.0),
					bezier_out: (0.0, 0.0),
				})
				.expect("animatedGain key");
		}

		let expected_at = [
			(Rational::new(0, 1), 0.5_f32),
			(Rational::new(1, 2), 1.0_f32),
			(Rational::new(1, 1), 1.5_f32),
		];
		let mut before_reload = Vec::new();
		for (time, expected_rgb) in expected_at {
			let rendered =
				crate::render::render_frame_f32(&session.project(), sequence, time, (8, 8))
					.expect("render animated frame");
			assert!(
				rendered
					.chunks_exact(4)
					.all(|px| (px[0] - expected_rgb).abs() < 1e-5
						&& (px[1] - expected_rgb).abs() < 1e-5
						&& (px[2] - expected_rgb).abs() < 1e-5
						&& (px[3] - 1.0).abs() < 1e-5)
			);
			before_reload.push(rendered);
		}

		let path = env::temp_dir()
			.join(format!("quercus_animated_gain_{}.ove", process::id()))
			.to_string_lossy()
			.into_owned();
		session.save(&path).expect("save");
		let opened = EditorSession::open(&path).expect("reopen");
		for ((time, _), prev) in expected_at.iter().zip(before_reload.iter()) {
			let reopened =
				crate::render::render_frame_f32(&opened.project(), sequence, *time, (8, 8))
					.expect("reopened render");
			assert_eq!(&reopened, prev, "reloaded frame mismatch at {time:?}");
		}
		let _ = fs::remove_file(path);
		let _ = fs::remove_file(media);
	}
}
