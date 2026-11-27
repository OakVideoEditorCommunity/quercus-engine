// Quercus Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Quercus Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Host-side Interact end-to-end: lifecycle + action call surface + Draw suite
//! real GL.
//!
//! Chain: scan the minimal test plugin's interact variant
//! (org.quercus.test-plugin.interact, which at describe time declares
//! kOfxImageEffectPluginPropOverlayInteractV2 → points at
//! mainEntryInteract) → `new_interact` (sends kOfxActionNewInteract) →
//! describe → create_instance → pen/key/idle events → draw (the host
//! gl_bridge keeps GL current; the plugin uses the Draw suite setColour +
//! native GL to draw a known colour block) → read back and assert.
//!
//! The assertions are the evidence that the plugin is really called: the
//! plugin writes each action's arguments into the marker file pointed to by
//! `QUERCUS_TEST_PLUGIN_INTERACT_MARKER` (on unix the `va_list` is provided
//! by stdarg.h), and the test asserts line by line; the GL assertions
//! additionally read back FBO pixels and compare the plugin's drawn colour
//! (distinguishable from the CPU path).
//!
//! Gates: the event tests do not depend on GL and run on any platform; the
//! draw tests require macOS + `QUERCUS_GPU_TESTS` (the GL acceptance
//! convention, always skipped in CI).

use quercus_ofx_plugin::gl_bridge;
use quercus_ofx_plugin::host;
use quercus_ofx_plugin::node_factory;
use quercus_ofx_plugin::render;
use std::env;
use std::fs;
use std::process;
mod common;

use std::path::PathBuf;

use quercus_ofx_plugin::suites::{interact::Interact, status};

const INTERACT_PLUGIN_ID: &str = "org.quercus.test-plugin.interact";
const BASE_PLUGIN_ID: &str = "org.quercus.test-plugin";
/// The plugin writes its interact action records to the file this
/// environment variable points to.
const MARKER_ENV: &str = "QUERCUS_TEST_PLUGIN_INTERACT_MARKER";

/// Scans the test plugin + registers the node (idempotent; unavailable →
/// false = skip).
fn scan_and_register() -> bool {
	let Some(dir) = common::test_plugin_scan_dir() else {
		common::skip("minimal test plugin not built");
		return false;
	};
	if host::Host::global().cache.scan_path(&dir).is_err() {
		common::skip("test plugin scan failed");
		return false;
	}
	node_factory::register_plugin_nodes();
	true
}

/// Standalone marker-file path (temp directory, process id to avoid
/// collisions).
fn marker_path(tag: &str) -> PathBuf {
	env::temp_dir().join(format!("quercus-interact-{}-{}.log", process::id(), tag))
}

/// Reads all lines of the marker file.
fn read_marker(path: &PathBuf) -> Vec<String> {
	fs::read_to_string(path)
		.unwrap_or_default()
		.lines()
		.map(|l| l.to_string())
		.collect()
}

/// Plugin without interact: new_interact → None (the main entry returns
/// ReplyDefault for kOfxActionNewInteract and declares no overlay entry).
#[test]
fn base_plugin_has_no_interact() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}
		let inst = host::Host::global()
			.create_instance(BASE_PLUGIN_ID, None)
			.expect("base plugin instance should be creatable");
		assert!(
			inst.value.new_interact().is_none(),
			"new_interact on a plugin without interact should be None"
		);
		host::Host::global().shutdown();
	});
}

/// Lifecycle + event call surface: new_interact → describe → create →
/// pen/key/idle → destroy. Asserts the plugin-side real records and arguments
/// one by one.
#[test]
fn interact_lifecycle_and_events() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}
		let marker = marker_path("events");
		let _ = fs::remove_file(&marker);
		unsafe { env::set_var(MARKER_ENV, &marker) };

		let inst = host::Host::global()
			.create_instance(INTERACT_PLUGIN_ID, None)
			.expect("interact variant instance should be creatable");

		let interact_arc = inst
			.value
			.new_interact()
			.expect("interact variant should create an interact");
		let interact: &Interact = &interact_arc;
		assert_eq!(interact.describe(), status::OK);
		assert_eq!(interact.create_instance(), status::OK);

		// Events: coordinates are viewport pixels; pixelScale defaults to 1 → canonical == viewport.
		assert_eq!(interact.pen_motion((10.0, 20.0), true, 5.0), status::OK);
		assert_eq!(interact.pen_down((30.0, 40.0), 5.0), status::OK);
		assert_eq!(interact.pen_up((30.0, 40.0), 5.0), status::OK);
		assert_eq!(interact.key_down(host::KEY_A, "a", 5.0), status::OK);
		assert_eq!(interact.key_up(host::KEY_A, "a", 5.0), status::OK);
		assert_eq!(interact.idle(), status::OK);
		interact.destroy();

		unsafe { env::remove_var(MARKER_ENV) };
		let lines = read_marker(&marker);
		let _ = fs::remove_file(&marker);

		// Lifecycle sequence (in order).
		let seq = ["new_interact", "describe", "create", "destroy"];
		let pos: Vec<Option<usize>> = seq
			.iter()
			.map(|s| lines.iter().position(|l| l == s))
			.collect();
		assert!(
			pos.iter().all(|p| p.is_some()),
			"marker file is missing lifecycle actions: {lines:?}"
		);
		let pos: Vec<usize> = pos.into_iter().map(|p| p.unwrap()).collect();
		assert!(
			pos.windows(2).all(|w| w[0] < w[1]),
			"lifecycle order should be new_interact->describe->create->destroy: {lines:?}"
		);

		// pen events: real parameters (viewport position/canonical/pressure; C %g has no trailing zeros).
		assert!(
			lines
				.iter()
				.any(|l| l == "pen_motion vp=10,20 canon=10,20 pressure=1"),
			"pen_motion arguments mismatch: {lines:?}"
		);
		assert!(
			lines
				.iter()
				.any(|l| l == "pen_down vp=30,40 canon=30,40 pressure=1"),
			"pen_down arguments mismatch: {lines:?}"
		);
		assert!(
			lines
				.iter()
				.any(|l| l == "pen_up vp=30,40 canon=30,40 pressure=0"),
			"pen_up arguments mismatch: {lines:?}"
		);

		// key events: real keySym/keyString.
		assert!(
			lines.iter().any(|l| l == "key_down sym=97 str=a"),
			"key_down arguments mismatch: {lines:?}"
		);
		assert!(
			lines.iter().any(|l| l == "key_up sym=97 str=a"),
			"key_up arguments mismatch: {lines:?}"
		);
		assert!(
			lines.iter().any(|l| l == "idle"),
			"idle was not recorded: {lines:?}"
		);

		host::Host::global().shutdown();
	});
}

/// Return values are passed through faithfully: the plugin returns
/// kOfxStatReplyDefault for Escape's key_down (unhandled) and the host returns
/// 14 unchanged.
#[test]
fn interact_passthrough_plugin_status() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}
		let marker = marker_path("status");
		let _ = fs::remove_file(&marker);
		unsafe { env::set_var(MARKER_ENV, &marker) };

		let inst = host::Host::global()
			.create_instance(INTERACT_PLUGIN_ID, None)
			.expect("interact variant instance should be creatable");
		let interact = inst
			.value
			.new_interact()
			.expect("interact should be creatable");

		// Escape key_down: plugin returns ReplyDefault → host passes it through.
		let st = interact.key_down(host::KEY_ESCAPE, "", 0.0);
		assert_eq!(
			st,
			status::REPLY_DEFAULT,
			"plugin Escape -> ReplyDefault should pass through"
		);

		unsafe { env::remove_var(MARKER_ENV) };
		let lines = read_marker(&marker);
		let _ = fs::remove_file(&marker);
		assert!(
			lines.iter().any(|l| l == "key_down sym=65307 str="),
			"Escape key_down should reach the plugin: {lines:?}"
		);

		host::Host::global().shutdown();
	});
}

/// Destroying the instance also destroys the interact (same lifecycle): drop
/// the instance → the plugin receives destroy (the interact's
/// kOfxActionDestroyInstance).
#[test]
fn instance_destroy_cleans_up_interact() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}
		let marker = marker_path("destroy");
		let _ = fs::remove_file(&marker);
		unsafe { env::set_var(MARKER_ENV, &marker) };

		let inst = host::Host::global()
			.create_instance(INTERACT_PLUGIN_ID, None)
			.expect("interact variant instance should be creatable");
		let _interact = inst
			.value
			.new_interact()
			.expect("interact should be creatable");

		// drop the instance (Arc refcount to zero → Drop → notify_destroy → interact destroy).
		drop(inst);

		unsafe { env::remove_var(MARKER_ENV) };
		let lines = read_marker(&marker);
		let _ = fs::remove_file(&marker);
		assert!(
			lines.iter().any(|l| l == "destroy"),
			"instance destruction should also destroy the interact: {lines:?}"
		);

		host::Host::global().shutdown();
	});
}

/// draw end-to-end (real GL): the host acquires → creates the output
/// texture/FBO → calls draw → the plugin glClears a dark background + Draw
/// suite setColour(0.9,0.1,0.2,1) + draw(Rectangle 10..30) → read back and
/// assert the rectangle area colour and the background colour.
///
/// Also asserts the Draw suite state really round-trips: setColour/getColour
/// return OK on the plugin side (recorded in the marker file).
#[test]
fn interact_draw_renders_plugin_colours() {
	common::with_host(|| {
		if !cfg!(target_os = "macos") {
			common::skip("GL bridge is only implemented on macOS");
			return;
		}
		if !common::gpu_available() {
			common::skip("GL tests require QUERCUS_GPU_TESTS");
			return;
		}
		if !gl_bridge::gl_available() {
			common::skip("no usable GL context on this machine");
			return;
		}
		if !scan_and_register() {
			return;
		}
		let marker = marker_path("draw");
		let _ = fs::remove_file(&marker);
		unsafe { env::set_var(MARKER_ENV, &marker) };

		let inst = host::Host::global()
			.create_instance(INTERACT_PLUGIN_ID, None)
			.expect("interact variant instance should be creatable");
		let interact = inst
			.value
			.new_interact()
			.expect("interact should be creatable");

		let (w, h) = (64i32, 64i32);
		let params = render::VideoParams {
			width: w,
			height: h,
			format: render::PIXEL_FORMAT_F32,
			..Default::default()
		};
		// One acquire covers FBO setup → draw (nested acquire inside) → read-back.
		let _guard = gl_bridge::acquire().expect("acquire should succeed");
		let tex = gl_bridge::create_output_texture(w, h, &params)
			.expect("output texture should be creatable");
		let fbo = gl_bridge::create_fbo(tex, w, h).expect("FBO should be complete");
		gl_bridge::bind_fbo(fbo);
		gl_bridge::set_viewport(w, h);

		let st = interact.draw((64.0, 64.0), (1.0, 1.0), 5.0, None);
		assert_eq!(st, status::OK, "draw should return OK from the plugin");

		let img = gl_bridge::read_pixels_to_image(w, h, &params).expect("read-back should succeed");
		gl_bridge::delete_fbo(fbo);
		gl_bridge::delete_gl_texture(tex);
		drop(_guard);

		unsafe { env::remove_var(MARKER_ENV) };
		let lines = read_marker(&marker);
		let _ = fs::remove_file(&marker);

		// Draw suite state really round-trips (recorded on the plugin side).
		assert!(
			lines.iter().any(|l| {
				l.starts_with("draw vp=64x64")
					&& l.contains("setcol_st=0")
					&& l.contains("setcol=0.9,0.1,0.2,1")
					&& l.contains("getcol_st=0")
					&& l.contains("getcol=0,0,0,1")
					&& l.contains("draw_st=0")
			}),
			"draw record mismatch (setcol/getcol/draw should all be OK): {lines:?}"
		);

		// Pixel assertion: the GL pixels of rect region (10..30)² → frame y 33..53 after flip.
		// F32 RGBA is 16 bytes per pixel (tight rows).
		let stride = (w as usize) * 16;
		let px = |x: usize, y: usize| -> [f32; 4] {
			let p = &img.data.as_slice()[y * stride + x * 16..y * stride + x * 16 + 16];
			[
				f32::from_ne_bytes(p[0..4].try_into().unwrap()),
				f32::from_ne_bytes(p[4..8].try_into().unwrap()),
				f32::from_ne_bytes(p[8..12].try_into().unwrap()),
				f32::from_ne_bytes(p[12..16].try_into().unwrap()),
			]
		};
		// The corner (outside the rect) should be the plugin's dark clear background.
		let corner = px(0, 0);
		for (i, v) in [0.05f32, 0.05, 0.05, 1.0].iter().enumerate() {
			assert!(
				(corner[i] - v).abs() < 1e-4,
				"corner pixel[{i}] = {}, expected dark background {v}",
				corner[i]
			);
		}
		// Inside the rect (frame (20,40) = GL (20,23)) should be the setColour colour.
		let inside = px(20, 40);
		for (i, v) in [0.9f32, 0.1, 0.2, 1.0].iter().enumerate() {
			assert!(
				(inside[i] - v).abs() < 1e-4,
				"pixel inside rectangle[{i}] = {}, expected {v}",
				inside[i]
			);
		}
		// The whole frame should contain only two colours (background + rect); the rect covers ~400 pixels.
		let (mut rect_n, mut clear_n) = (0usize, 0usize);
		for y in 0..h as usize {
			for x in 0..w as usize {
				let c = px(x, y);
				let is_rect = (0..4).all(|i| (c[i] - [0.9, 0.1, 0.2, 1.0][i]).abs() < 1e-3);
				let is_clear = (0..4).all(|i| (c[i] - [0.05, 0.05, 0.05, 1.0][i]).abs() < 1e-3);
				if is_rect {
					rect_n += 1;
				} else if is_clear {
					clear_n += 1;
				} else {
					panic!("unexpected colour pixel ({x},{y}): {c:?}");
				}
			}
		}
		assert!(
			(rect_n as i32 - 400).abs() <= 16,
			"rectangle should cover ~400 pixels, got {rect_n}"
		);
		assert_eq!(
			rect_n + clear_n,
			(w * h) as usize,
			"the whole frame should contain only the background and rectangle colours"
		);

		host::Host::global().shutdown();
	});
}
