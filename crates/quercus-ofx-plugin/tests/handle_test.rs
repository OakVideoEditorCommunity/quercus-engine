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

//! Contract tests for handle.rs: the `RefBox` container.
//!
//! After the single-library change (M14 R5) the crate no longer passes CHandle internally — the old CHandle packing/unpacking
//! (`make_owned`/`make_borrowed`/`get`), panic fallback (`guard`/
//! `guard_handle`/`guard_void`) and identity registry (`Registry`) are all removed.
//! [`quercus_ofx_plugin::handle::RefBox`] is kept only as the boundary return type of `Host::create_instance`
//! (`Arc<RefBox<Instance>>`, quercusengine test_support
//! consumes it); here we verify its basic contract. Each test checks one rule; the name is the spec.

mod common;

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use quercus_ofx_plugin::handle::RefBox;

/// Drop flag: asserts the object's destruction timing by counting destructions (a behavioral
/// probe for Arc lifetime semantics).
struct DropFlag(Arc<AtomicUsize>);

impl Drop for DropFlag {
	fn drop(&mut self) {
		self.0.fetch_add(1, Ordering::Relaxed);
	}
}

/// The value is retrievable through the `.value` field (how the caller accesses it after create_instance returns).
#[test]
fn refbox_exposes_value_by_field() {
	let boxed = RefBox {
		refs: AtomicU32::new(1),
		value: 7u32,
	};
	assert_eq!(boxed.value, 7);
}

/// Lifetime is managed entirely by the outer Arc: `refs` is only ever constructed with the fixed value 1 (no thunk
/// increments/decrements), and when the Arc reaches zero the contained object is dropped exactly once; the weak reference stays valid while the Arc lives
/// and expires after it reaches zero (the host's active-instance table is such a Weak list).
#[test]
fn refbox_value_drops_exactly_once_when_arc_reaches_zero() {
	let drops = Arc::new(AtomicUsize::new(0));
	let boxed = Arc::new(RefBox {
		refs: AtomicU32::new(1),
		value: DropFlag(drops.clone()),
	});

	let weak = Arc::downgrade(&boxed);
	assert!(weak.upgrade().is_some());
	assert_eq!(drops.load(Ordering::Relaxed), 0);

	drop(boxed);
	assert_eq!(drops.load(Ordering::Relaxed), 1);
	assert!(weak.upgrade().is_none());
}
