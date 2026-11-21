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

//! Test common items: minimal test-plugin location, snapshot/golden file paths, fixtures.
//!
//! The minimal test plugin (cbits/quercus_test_plugin.c, compiled by build.rs into a shared library,
//! assembled at runtime into quercus-test-plugin.ofx.bundle): filter context,
//! a Double parameter gain, two clips (Source/Output). When the plugin is not built, the relevant
//! cases return early through [`skip`].
//! After the single-library change the pixel path is driven by the quercusrender value model (`quercus_core::texture::Texture`);
//! render goldens await that migration.

// Each integration-test binary compiles this module independently and uses only the helpers it needs,
// so items unused by one binary still belong to the shared fixture API.
#![allow(dead_code)]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync;

/// Name of the environment variable through which the build system injects the plugin path.
pub const TEST_PLUGIN_ENV: &str = "QUERCUS_TEST_PLUGIN_DIR";

/// Absolute path of the test-plugin bundle (injected by the build system via an environment variable;
/// when not injected, the bundle directory is assembled on the spot from the shared library built by build.rs; when neither is usable
/// it returns None and the caller skips).
pub fn test_plugin_dir() -> Option<PathBuf> {
	if let Some(p) = env::var_os(TEST_PLUGIN_ENV) {
		return Some(PathBuf::from(p));
	}
	static BUNDLE: sync::OnceLock<Option<PathBuf>> = sync::OnceLock::new();
	BUNDLE
		.get_or_init(|| {
			let out = PathBuf::from(env!("OUT_DIR"));
			let lib = if cfg!(target_os = "macos") {
				out.join("quercus_test_plugin.dylib")
			} else {
				out.join("quercus_test_plugin.so")
			};
			if !lib.is_file() {
				return None;
			}
			let bundle = env::temp_dir()
				.join(format!("quercus-test-plugin-{}", process::id()))
				.join("quercus-test-plugin.ofx.bundle");
			let platform = if cfg!(target_os = "macos") {
				"MacOS"
			} else if cfg!(target_os = "windows") {
				"Win64"
			} else {
				"Linux-x86-64"
			};
			let bin_dir = bundle.join("Contents").join(platform);
			fs::create_dir_all(&bin_dir).ok()?;
			// Windows requires the .dll extension: LoadLibrary automatically appends
			// ".dll" to an extension-less module name, so a file named "plugin" fails to load.
			let target = bin_dir.join(if cfg!(target_os = "windows") {
				"plugin.dll"
			} else {
				"plugin"
			});
			if !target.exists() {
				fs::copy(&lib, &target).ok()?;
			}
			Some(bundle)
		})
		.clone()
}

/// Parent directory of the test-plugin bundle (the input of host_scan).
pub fn test_plugin_scan_dir() -> Option<PathBuf> {
	let bundle = test_plugin_dir()?;
	let parent = bundle.parent()?;
	// scan_path canonicalizes; the bundle lives in the temp dir, so hand over the parent directly.
	Some(parent.to_path_buf())
}

/// Golden master directory (tests/ofx/). The descriptor snapshot JSON and the EXR frames live here.
pub fn golden_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/ofx")
}

/// Whether the current platform has a GPU (the gate for GL golden cases: without a GPU, always skip).
///
/// There are no GL cases in phase 1: enabled by an explicit environment variable (GL acceptance needs manual local confirmation;
/// CI always skips).
pub fn gpu_available() -> bool {
	env::var_os("QUERCUS_GPU_TESTS").is_some()
}

/// Function form of the common skip macro: prints the reason and returns early when a precondition is not met.
/// (cargo has no GTEST_SKIP; the convention is `return`, with SKIP printed in the output.)
pub fn skip(reason: &str) {
	println!("SKIP: {reason}");
}

/// Serialize host-singleton tests: tests in the same binary would otherwise trample each other's
/// init/shutdown/scan (the process singleton has no lock); every case that touches the host surface goes through this.
pub fn with_host(f: impl FnOnce()) {
	static LOCK: sync::Mutex<()> = sync::Mutex::new(());
	let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
	f();
}
