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

//! push button → `kOfxActionInstanceChanged` routing (end to end).
//!
//! Chain: `scan_path` (a minimal test plugin, cbits/quercus_test_plugin.c,
//! whose describe defines a push button parameter "button"; on
//! instanceChanged it appends a line to the file named by
//! `QUERCUS_TEST_PLUGIN_INSTANCECHANGED_MARKER`) → `create_instance` →
//! `push_button_clicked` (set the value + route a UserEdited instanceChanged)
//! → the plugin records the callback count.
//!
//! Skipped when the test plugin was not built (the common convention). The
//! host singleton is serialized through `common::with_host`.

use quercus_ofx_plugin::node_factory;
use quercus_ofx_plugin::param;
use std::env;
use std::fs;
use std::process;
mod common;

use quercus_ofx_plugin::host::Host;

const PLUGIN_ID: &str = "org.quercus.test-plugin";

#[test]
fn push_button_press_routes_instance_changed() {
	common::with_host(|| {
		let Some(dir) = common::test_plugin_scan_dir() else {
			common::skip("minimal test plugin not built");
			return;
		};
		if Host::global().cache.scan_path(&dir).is_err() {
			common::skip("test plugin scan failed");
			return;
		}
		let inst = Host::global()
			.create_instance(PLUGIN_ID, None)
			.expect("instance");
		let id = node_factory::register_instance(inst.clone());

		// The C plugin appends one line per instanceChanged("button") to the
		// marker file; the assertion reads it back.
		let marker = env::temp_dir().join(format!("quercus-push-{}.log", process::id()));
		let _ = fs::remove_file(&marker);
		env::set_var("QUERCUS_TEST_PLUGIN_INSTANCECHANGED_MARKER", &marker);

		// The button param exists and is a push button.
		let p = inst.value.params.find("button").expect("button parameter");
		assert_eq!(p.def.ofx_type, param::TYPE_PUSHBUTTON);

		// First press: set + routed to the plugin.
		assert!(node_factory::push_button_clicked(id, "button"));
		// Unknown / non-button params are rejected without touching the entry.
		assert!(!node_factory::push_button_clicked(id, "gain"));
		assert!(!node_factory::push_button_clicked(id, "nope"));
		assert!(!node_factory::push_button_clicked(u64::MAX, "button"));
		// Second press on the real button: routed again.
		assert!(node_factory::push_button_clicked(id, "button"));

		// Exactly the two real presses reached the plugin's instanceChanged.
		let log = fs::read_to_string(&marker).expect("marker file should exist");
		assert_eq!(log.lines().count(), 2, "marker log:\n{log}");
		assert!(log.contains("instanceChanged"), "marker log:\n{log}");

		env::remove_var("QUERCUS_TEST_PLUGIN_INSTANCECHANGED_MARKER");
		let _ = fs::remove_file(&marker);
		node_factory::unregister_instance(id);
		Host::global().shutdown();
	});
}
