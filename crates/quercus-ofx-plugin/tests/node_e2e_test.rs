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

//! Stage 6a end-to-end: OFX plugin → node factory → node graph → rendered
//! frame.
//!
//! Chain: `scan_path` (minimal test plugin, cbits/quercus_test_plugin.c) →
//! `node_factory::register_plugin_nodes` (dynamic registration) →
//! `Factory::create_any` (the input table of parameter translation) → Graph
//! connected to a constant texture source → `Traverser::evaluate` +
//! `RenderEvalHooks` (resolves PluginJobPayload and renders a frame through
//! render_driver) → pixel assertions.
//!
//! All tests skip when the test plugin is not built (the common convention).
//! The host singleton is serialized through `common::with_host`.

use quercus_node::handle;
use quercus_node::input;
use quercus_node::nodes;
use quercus_ofx_plugin::node_factory;
use quercus_ofx_plugin::param;
use quercus_ofx_plugin::render_driver;
use quercus_render::eval;
mod common;

use quercus_core::texture::Texture;
use quercus_core::{PixelFormat, Rational};
use quercus_node::factory::Factory;
use quercus_node::graph::Graph;
use quercus_node::node::{NodeBehavior, NodeCore};
use quercus_node::traverser::{EvalRequest, Traverser};
use quercus_node::value::{NodeValue, ValueType};
use quercus_ofx_plugin::host::Host;

const PLUGIN_ID: &str = "org.quercus.test-plugin";
const IDENTITY_ID: &str = "org.quercus.test-plugin.identity";

/// Constant texture source node: pushes one F32 frame filled with a solid
/// color (test-only behavior).
struct ConstSource {
	rgba: [f32; 4],
	size: (i32, i32),
}

impl NodeBehavior for ConstSource {
	fn name(&self) -> &str {
		"ConstSource"
	}

	fn type_id(&self) -> &str {
		"test.const-source"
	}

	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(ConstSource {
			rgba: self.rgba,
			size: self.size,
		}))
	}

	fn value(
		&self,
		_core: &NodeCore,
		_inputs: &quercus_node::value::NodeValueRow,
		time: Rational,
		table: &mut quercus_node::value::NodeValueTable,
	) {
		let mut frame = eval::generate_frame(time, self.size, PixelFormat::F32).unwrap();
		for pixel in frame.data.as_chunks_mut::<16>().0 {
			for (i, v) in self.rgba.iter().enumerate() {
				pixel[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
			}
		}
		table.push(
			ValueType::Texture,
			NodeValue::Texture(handle::make_owned(Texture::wrap_frame(frame))),
			None,
		);
	}
}

/// Scans and registers (idempotent; returns false = skip when the host is
/// unavailable).
fn scan_and_register() -> bool {
	let Some(dir) = common::test_plugin_scan_dir() else {
		common::skip("minimal test plugin not built");
		return false;
	};
	if Host::global().cache.scan_path(&dir).is_err() {
		common::skip("test plugin scan failed");
		return false;
	}
	node_factory::register_plugin_nodes();
	true
}

/// Takes the render texture out of the output table (the real texture box
/// after resolve).
fn rendered_texture(table: &quercus_node::value::NodeValueTable) -> Texture {
	let NodeValue::Texture(handle) = table
		.get(ValueType::Texture)
		.expect("root output should have a texture")
	else {
		panic!("texture slot is not a Texture value");
	};
	unsafe { handle::get_checked::<Texture>(handle) }
		.cloned()
		.expect("texture box must be the render product (PluginJobPayload already resolved)")
}

fn first_pixel(texture: &Texture) -> [f32; 4] {
	let Texture::Cpu(frame) = texture else {
		panic!("expected a CPU frame");
	};
	let mut out = [0f32; 4];
	for i in 0..4 {
		out[i] = f32::from_le_bytes(frame.data[i * 4..i * 4 + 4].try_into().unwrap());
	}
	out
}

/// Registration + parameter translation: dynamic entries, input
/// type/default/display name/hidden flag/combo options/effect_input (mirrors
/// the plugin.cpp constructor).
#[test]
fn plugin_nodes_register_with_translated_inputs() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}

		let entries = Factory::global().dynamic_entries();
		assert!(
			entries.iter().any(|m| m.type_id == PLUGIN_ID),
			"CPU variant should be registered as a dynamic node"
		);
		assert!(
			entries.iter().any(|m| m.type_id == IDENTITY_ID),
			"identity variant should be registered as a dynamic node"
		);
		let meta = entries.iter().find(|m| m.type_id == PLUGIN_ID).unwrap();
		assert_eq!(meta.sub_category, "Filter");
		assert_eq!(meta.categories, vec![quercus_node::node::Category::OpenFx]);

		let (core, behavior) = Factory::global()
			.create_any(PLUGIN_ID)
			.expect("create_any should create the plugin node");

		// gain: Double → Float, default 0.0, display name Gain, with display
		// min/max properties (-2/2).
		let gain = core.get_input("gain").expect("gain input");
		assert_eq!(gain.value_type, ValueType::Float);
		assert_eq!(gain.default, NodeValue::Float(0.0));
		assert_eq!(gain.display_name, "Gain");
		// The C++ only sets min/max/tooltip properties for colour inputs
		// (the k_color branch in plugin.cpp:407-430); Double parameters have none.
		assert!(gain
			.properties
			.iter()
			.all(|(k, _)| k != "min" && k != "max"));

		// mode: Choice → Combo, two options Fast/High.
		let mode = core.get_input("mode").expect("mode input");
		assert_eq!(mode.value_type, ValueType::Combo);
		let options: Vec<String> = mode
			.properties
			.iter()
			.filter(|(k, _)| k == "combo_option")
			.map(|(_, v)| match v {
				NodeValue::Text(s) => s.clone(),
				_ => panic!("combo_option should be Text"),
			})
			.collect();
		assert_eq!(options, vec!["Fast".to_string(), "High".to_string()]);

		// debug: secret → hidden.
		let debug = core.get_input("debug").expect("debug input");
		assert!(
			debug.flags & input::flags::HIDDEN != 0,
			"secret parameter should be hidden"
		);

		// label: String → Text.
		let label = core.get_input("label").expect("label input");
		assert_eq!(label.value_type, ValueType::Text);

		// Source clip → texture input; effect_input selects Source.
		let source = core.get_input("Source").expect("Source input");
		assert_eq!(source.value_type, ValueType::Texture);
		assert_eq!(core.effect_input, "Source");

		// The behavior is a PluginNode holding a real instance handle.
		let plugin = behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<nodes::plugin::PluginNode>())
			.expect("behavior should be a PluginNode");
		assert!(!plugin.instance_handle().is_null());

		Host::global().shutdown();
	});
}

/// CPU end-to-end: constant source → plugin node (render fills constant
/// 0.5/alpha 1) → pixel assertions on the output frame.
#[test]
fn plugin_renders_constant_frame_end_to_end() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}
		let (core, behavior) = Factory::global().create_any(PLUGIN_ID).expect("create_any");

		let mut graph = Graph::new();
		let src_id = graph.add_node(
			NodeCore::new(),
			Box::new(ConstSource {
				rgba: [0.2, 0.4, 0.6, 1.0],
				size: (4, 4),
			}),
		);
		let plug_id = graph.add_node(core, behavior);
		graph
			.connect(src_id, plug_id, "Source", -1)
			.expect("Source connection");

		let mut traverser = Traverser::new();
		let mut hooks = eval::RenderEvalHooks::new();
		let table = traverser
			.evaluate(
				&graph,
				&EvalRequest::new(plug_id, Rational::new(0, 1)),
				&mut hooks,
			)
			.expect("evaluate should succeed");

		let texture = rendered_texture(&table);
		assert_eq!(texture.size(), (4, 4));
		// The test plugin's render ignores its inputs and fills the constant 0.5 (alpha=1).
		assert_eq!(first_pixel(&texture), [0.5, 0.5, 0.5, 1.0]);

		Host::global().shutdown();
	});
}

/// isIdentity pass-through: the identity variant declares it always passes
/// Source through → the output should equal the input frame (render_driver's
/// passthrough short-circuit).
#[test]
fn identity_variant_passes_source_through() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}
		let (core, behavior) = Factory::global()
			.create_any(IDENTITY_ID)
			.expect("create_any(identity)");

		let mut graph = Graph::new();
		let src_id = graph.add_node(
			NodeCore::new(),
			Box::new(ConstSource {
				rgba: [0.25, 0.75, 0.5, 1.0],
				size: (2, 2),
			}),
		);
		let plug_id = graph.add_node(core, behavior);
		graph
			.connect(src_id, plug_id, "Source", -1)
			.expect("Source connection");

		let mut traverser = Traverser::new();
		let mut hooks = eval::RenderEvalHooks::new();
		let table = traverser
			.evaluate(
				&graph,
				&EvalRequest::new(plug_id, Rational::new(0, 1)),
				&mut hooks,
			)
			.expect("evaluate should succeed");

		let texture = rendered_texture(&table);
		assert_eq!(first_pixel(&texture), [0.25, 0.75, 0.5, 1.0]);

		Host::global().shutdown();
	});
}

/// Parameter override path: Text/StrCombo go through set_ofx, numerics go
/// through POD — after a set the instance parameter values can be read back
/// (regression protection for the translation injection).
#[test]
fn param_overrides_reach_instance() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}
		let inst = Host::global()
			.create_instance(PLUGIN_ID, None)
			.expect("instance");
		let id = node_factory::register_instance(inst.clone());

		// Numeric override (gain = 1.25).
		let job_values = [(
			"gain".to_string(),
			quercus_node::value::NodeValue::Float(1.25),
		)];
		let pod: Vec<(String, quercus_ofx_plugin::node::Value)> = job_values
			.iter()
			.filter_map(|(k, v)| {
				quercus_ofx_plugin::node::Value::from_node_value(v).map(|p| (k.clone(), p))
			})
			.collect();
		let dst = eval::generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
		let src = eval::generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
		let job = render_driver::RenderJob {
			time: 0.0,
			dst: Texture::wrap_frame(dst),
			src: Some(Texture::wrap_frame(src)),
			effect_input_id: Some("Source".into()),
			inputs: Vec::new(),
			values: pod,
			renderer: None,
			clear_destination: false,
			interactive: false,
		};
		render_driver::render_frame(&inst.value, &job).expect("render_frame should succeed");
		let gain = inst.value.params.find("gain").unwrap().get();
		assert_eq!(gain, param::ParamValue::Double([1.25, 0.0, 0.0], 1));

		// NaN override falls back to the default (gain defaults to 0.0).
		let pod_nan = vec![(
			"gain".to_string(),
			quercus_ofx_plugin::node::Value::float(f64::NAN),
		)];
		let dst = eval::generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
		let src = eval::generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
		let job = render_driver::RenderJob {
			time: 0.0,
			dst: Texture::wrap_frame(dst),
			src: Some(Texture::wrap_frame(src)),
			effect_input_id: Some("Source".into()),
			inputs: Vec::new(),
			values: pod_nan,
			renderer: None,
			clear_destination: false,
			interactive: false,
		};
		render_driver::render_frame(&inst.value, &job).expect("NaN override should not fail");
		assert_eq!(
			inst.value.params.find("gain").unwrap().get(),
			param::ParamValue::Double([0.0, 0.0, 0.0], 1)
		);

		node_factory::unregister_instance(id);
		Host::global().shutdown();
	});
}
