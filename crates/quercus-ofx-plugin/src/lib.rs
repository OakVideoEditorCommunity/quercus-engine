//! **Internal implementation detail — do not depend on this crate directly.**
//!
//! It is published only for `quercus-engine`'s dependency graph. Its API is
//! not stable and may change without notice. Use `quercus-engine-rs`, or the
//! C ABI via `quercus-engine`, instead.

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

//! # quercusplugin — Quercus's plugin module (in-house OFX host)
//!
//! This crate is the full implementation of the quercusplugin module (M11):
//!
//! - **OFX host**: scans bundles, loads plugins, implements the eight suites
//!   (property/memory/image_effect/param/message/progress/timeline/
//!   multithread), and drives actions such as describe/createInstance/render.
//! - **Bridges**: wires OFX instance parameters to quercusnode ([`node`]:
//!   node-value PODs + the identity registry and undoable write-backs),
//!   wires clip inputs/outputs to quercusrender textures ([`render`]: the
//!   texture/frame/renderer value types), and wraps parameter changes as undo
//!   commands (directly through `quercus_undo::undocommand::UndoCommand`,
//!   see [`instance::Instance::submit_undo_command`]).
//!
//! ## Single-lib unification
//!
//! The C ABI exports (formerly [`ffi`]) and the module bridges (formerly
//! [`bridge`]) have been removed from this crate: quercusnode/quercusrender/
//! quercusundo are linked directly as path dependencies. The bridges are
//! rebuilt with direct Rust types ([`node`]'s identity registry and
//! `set_input_*_undoable`; [`render`]'s `Texture`/`Frame`/`Renderer` value
//! types). GL texture names (the GL namespace of the old
//! `quercusrender_texture_id`) are provided by [`gl_bridge`] (plan B:
//! offscreen CGL context + read-back); [`render::texture_id`] stays constant
//! 0 for quercusrender textures (wgpu/Metal have no GL namespace), and the
//! use_opengl decision plus the GL suite's OpenGLTextureIndex take real names
//! from [`gl_bridge`] instead.
//!
//! ## Handle discipline (the highest-priority convention crate-wide)
//!
//! 1. A plugin may call back into the suites from any thread it starts (for
//!    the lifetime of the multithread suite); all shared state goes through
//!    `Mutex`.
//! 2. OFX semantics take openfx HostSupport as the reference; negotiation
//!    and ordering implementation points must comment the corresponding
//!    HostSupport file and line number (format:
//!    `// HS: ofxhImageEffect.cpp:2776`).
//!
//! ## Phase 1 scope
//!
//! filter/generator/transition contexts; the CPU render path. The GL texture
//! suite and ofxColour are phase 2 (reserved points are marked with
//! `// [P2]` inside [`clip`]/[`image`]).

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod clip;
pub mod descriptor;
pub mod error;
pub mod gl_bridge;
pub mod handle;
pub mod host;
pub mod image;
pub mod instance;
pub mod node;
pub mod node_factory;
pub mod param;
pub mod param_curve;
pub mod progress;
pub mod property;
pub mod render;
pub mod render_driver;
pub mod suites;

/// The minimal test plugin shared library built by this crate's build
/// script (`$OUT_DIR/quercus_test_plugin.{so,dylib}`), for integration tests
/// that exercise the real plugin path — including the single OFX host
/// process (M3) — without a system plugin. The build script always
/// compiles it, so this is `None` only if the build directory was
/// tampered with (tests treat that as a hard failure, not a skip).
pub fn bundled_test_plugin() -> Option<std::path::PathBuf> {
	let dir = std::path::Path::new(env!("OUT_DIR"));
	for name in [
		"quercus_test_plugin.dylib",
		"quercus_test_plugin.so",
		"quercus_test_plugin.dll",
	] {
		let path = dir.join(name);
		if path.exists() {
			return Some(path);
		}
	}
	None
}
