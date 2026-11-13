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

//! A historical remnant of the handle machinery: the `RefBox<T>` container.
//!
//! After the single-library unification (M14 R5) CHandle is no longer passed
//! inside the crate — the original CHandle boxing/unboxing
//! (`make_owned`/`make_borrowed`/`get`), panic guards (`guard`/
//! `guard_handle`/`guard_void`) and identity registry (`Registry`) have all
//! been deleted: the former was only used by tests, and the latter had no
//! readers in src (the param bridge actually goes through
//! [`crate::suites::param`]'s props address mapping and [`crate::node`]'s
//! identity registry).
//!
//! Only [`RefBox`] remains: it is the return-value container of
//! [`crate::host::Host::create_instance`] (`Arc<RefBox<Instance>>`), and
//! quercusengine's test_support explicitly marks that type as consumed, so it
//! is this crate's public type at the facade boundary.

use std::sync::atomic::AtomicU32;

/// Boundary box: the carrier type for `Arc<RefBox<Instance>>`.
///
/// Originally the heap box behind a C handle (`{ctx, addref, release,
/// abi_version}`), with addref/release thunks counting through `refs`; after
/// the boxing/unboxing removal `refs` is no longer read or written and is
/// constructed with the fixed value 1 ("single owner" semantics), with the
/// lifetime managed entirely by the outer `Arc`.
pub struct RefBox<T: ?Sized> {
	/// Reference count (a handle-era leftover; no thunk reads or writes it now).
	pub refs: AtomicU32,
	/// The contained object.
	pub value: T,
}
