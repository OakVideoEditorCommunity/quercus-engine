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

//! OFX property sets (the host-side storage for the Property Suite).
//!
//! Every OFX object (host/plugin/descriptor/instance/clip/image/param)
//! carries a [`PropertySet`]. Properties are multi-dimensional arrays
//! accessed by type.
//! Reference: HS: ofxhProperty.cpp (the GetSuite read/write semantics:
//! out-of-range returns kOfxStatErrBadIndex, a type mismatch returns
//! kOfxStatErrBadHandle).

use std::sync::Mutex;

use crate::error::{Error, Result};

/// A property value (single element). Strings are held with OFX's `char*`
/// semantics (owned by the host, borrowed by the plugin).
#[derive(Clone, Debug)]
pub enum Value {
	/// 32-bit integer.
	Int(i32),
	/// Double precision.
	Double(f64),
	/// UTF-8 string (a NUL-terminated `char*` on the OFX side).
	String(std::ffi::CString),
	/// Opaque pointer (e.g. `OfxImageEffectHandle` cross-references).
	Pointer(*mut std::ffi::c_void),
}

// 裸指针默认禁 Send/Sync；但属性集语义把指针当作**不透明令牌**：
// 跨线程只搬运/比较值，解引用永远是 suite 层的 unsafe 责任（OFX
// multithread suite 本就要求插件线程可读写宿主图像缓冲——指针的
// 跨线程传递是规范语义，而非逃逸）。Mutex 包裹后属性集整体可共享。
// 与 Rust 安全模型不冲突：安全代码只能拿到 `&Value`，无法解引用指针。
unsafe impl Send for Value {}
unsafe impl Sync for Value {}

/// A property: a name plus a multi-dimensional array of values.
#[derive(Clone, Debug)]
pub struct Property {
	/// OFX property name (kOfxProp*; owned — required by dynamic
	/// negotiation-time names such as
	/// "OfxImageClipPropComponents_<clip>", which `&'static str` cannot
	/// express).
	pub name: String,
	/// Element array; dimension = len.
	pub values: Vec<Value>,
}

/// A property set. Thread-safe (internal Mutex); an OFX object's `*Handle`
/// is the wrapper pointing at it.
pub struct PropertySet {
	props: Mutex<Vec<Property>>,
}

/// Deep copy (clones the inner array while holding the lock; needed for
/// descriptor properties → instance properties on createInstance).
impl Clone for PropertySet {
	fn clone(&self) -> Self {
		let props = lock(&self.props);
		Self {
			props: Mutex::new(props.clone()),
		}
	}
}

impl PropertySet {
	/// Empty set.
	pub fn new() -> Self {
		Self {
			props: Mutex::new(Vec::new()),
		}
	}

	/// Defines (or wholly replaces) a property. When a property with the same
	/// name exists, its value array is replaced.
	///
	/// `name` is the property name (case-sensitive exact match) and `values`
	/// becomes the property's element array as a whole, its length being the
	/// dimension; an empty `values` yields a zero-dimensional property. The
	/// property is created when absent (dimension and values both come from
	/// `values`).
	///
	/// Mirrors the replace semantics of C++ `Set::addProperty`
	/// (HS: ofxhPropertySuite.cpp:462) and the whole-array write semantics of
	/// `PropertyTemplate::setValueN`.
	///
	/// The method takes the internal lock, so `&self` suffices to mutate;
	/// concurrent calls are serialized.
	pub fn define(&self, name: &str, values: Vec<Value>) {
		let mut props = lock(&self.props);
		if let Some(p) = props.iter_mut().find(|p| p.name == name) {
			p.values = values;
		} else {
			props.push(Property {
				name: name.to_string(),
				values,
			});
		}
	}

	/// Convenience single-element definition.
	///
	/// Equivalent to [`Self::define`]`(name, vec![value])`: creates a
	/// one-dimensional property when absent, and replaces the whole value
	/// array with that single element when it exists (shrinking the dimension
	/// to 1).
	pub fn set_one(&self, name: &str, value: Value) {
		self.define(name, vec![value]);
	}

	/// Reads the `index`-th element; returns `None` when the property is
	/// absent or the index is out of range.
	///
	/// Out of range corresponds to C++'s kOfxStatErrBadIndex
	/// (HS: ofxhPropertySuite.cpp:257 `getValueRaw`), expressed here as an
	/// `Option`.
	pub fn get(&self, name: &str, index: usize) -> Option<Value> {
		let props = lock(&self.props);
		props
			.iter()
			.find(|p| p.name == name)
			.and_then(|p| p.values.get(index).cloned())
	}

	/// Overwrites the value of the `index`-th element (without changing the
	/// dimension); on failure returns [`crate::error::Error::NotFound`].
	///
	/// `name` is the property name, `index` is a subscript within
	/// `0..dimension(name)` and `value` overwrites that slot. Both an absent
	/// property and an out-of-range index return `NotFound` (corresponding to
	/// C++'s kOfxStatErrBadIndex).
	///
	/// The fixed dimension is deliberate: unlike C++ `setValue`'s automatic
	/// growth, here growth can only happen explicitly through
	/// [`PropertySet::define`] (the definer controls the dimension semantics,
	/// preventing a plugin from accidentally enlarging the array).
	pub fn set_at(&self, name: &str, index: usize, value: Value) -> Result<()> {
		let mut props = lock(&self.props);
		let p = props
			.iter_mut()
			.find(|p| p.name == name)
			.ok_or(Error::NotFound)?;
		let slot = p.values.get_mut(index).ok_or(Error::NotFound)?;
		*slot = value;
		Ok(())
	}

	/// Dimension (0 when the property is absent).
	///
	/// Returns the current length of the property's value array, i.e. the
	/// valid subscript upper bound `0..len` for [`Self::set_at`]. A missing
	/// name also returns 0, so "absent" and "zero-dimensional property"
	/// cannot be distinguished — use [`Self::get`] when that matters.
	pub fn dimension(&self, name: &str) -> usize {
		lock(&self.props)
			.iter()
			.find(|p| p.name == name)
			.map_or(0, |p| p.values.len())
	}

	/// Removes a property; a no-op when absent.
	///
	/// `name` is the property's full name (case-sensitive exact match). Only
	/// the matching entry is removed and the other properties keep their
	/// order; a missing name returns immediately without error. Afterwards
	/// the name reads as dimension 0 through [`Self::dimension`].
	pub fn remove(&self, name: &str) {
		lock(&self.props).retain(|p| p.name != name);
	}

	/// Iteration snapshot (used by dumps/snapshot tests).
	pub fn snapshot(&self) -> Vec<Property> {
		lock(&self.props).clone()
	}

	/// Suite-layer only: accesses the raw storage while holding the lock.
	///
	/// The property suite needs consistency semantics spanning multiple reads
	/// and writes (type check first, then indexed read/write, returning
	/// pointers to resident strings) that are compared against
	/// HS: ofxhProperty.cpp case by case; the public API cannot express them
	/// without re-locking. This entry hands the `Vec<Property>` to the suite
	/// implementation inside a single critical section. Visible only within
	/// this crate.
	pub(crate) fn with_locked<R>(&self, f: impl FnOnce(&mut Vec<Property>) -> R) -> R {
		let mut props = lock(&self.props);
		f(&mut props)
	}
}

/// Takes the lock. On a poisoned lock (crate code panicked while holding it)
/// its inner state is taken over and used — the property set's data itself is
/// always intact, so continuing is preferable to letting one panic cascade
/// into failures of all later FFI calls.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}
