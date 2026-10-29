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

//! # quercustask — the task execution module (Rust)
//!
//! Reimplements the C++ task module (`src/task/src`) as direct Rust
//! (single-lib unification): inheritance → one module per class + trait
//! objects, cancellation via `quercus_core::cancelatom::CancelAtom`, events
//! as boxed callbacks. All node-graph, timeline and render work goes
//! through the direct `quercusnode` / `quercusrender` Rust APIs
//! ([`nodeops`] holds the graph operations the tasks share).

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod codecbridge;
pub mod conform;
pub mod customcache;
pub mod error;
pub mod export;
pub mod handle;
pub mod manager;
pub mod nodeops;
pub mod precache;
pub mod project;
pub mod proxy;
pub mod render;
pub mod task;
