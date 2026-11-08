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

//! Error codes. These mirror `include/*/error.h` verbatim; the project
//! uses the unified -MMCCCC scheme (module-number registry in
//! include/common/error.h) and forwards codes across modules without
//! translating them.

use thiserror::Error;

/// Success.
pub const QUERCUSPLUGIN_OK: i32 = 0;
/// Null handle or invalid argument.
pub const QUERCUSPLUGIN_E_INVALID: i32 = -90001;
/// The call is not allowed in the current state (e.g. creating an
/// instance before scanning).
pub const QUERCUSPLUGIN_E_STATE: i32 = -90002;
/// The underlying operation failed (an OFX action returned
/// non-`kOfxStatOK`, the plugin entry point refused, etc.).
pub const QUERCUSPLUGIN_E_FAILED: i32 = -90003;
/// Index out of range / no plugin with the given identifier.
pub const QUERCUSPLUGIN_E_NOT_FOUND: i32 = -90004;
/// Allocation failed.
pub const QUERCUSPLUGIN_E_NOMEM: i32 = -90005;

/// The crate-wide result type; the FFI layer maps it to the i32 codes
/// above.
pub type Result<T> = std::result::Result<T, Error>;

/// A crate-internal error. `code()` returns the externally visible
/// error code.
#[derive(Debug, Error)]
pub enum Error {
	/// Null handle or invalid argument.
	#[error("plugin: invalid argument")]
	Invalid,
	/// Not allowed in the current state.
	#[error("plugin: call not valid in the current state")]
	State,
	/// Underlying failure with human-readable context (logged only; never
	/// crosses the ABI).
	#[error("plugin: operation failed: {0}")]
	Failed(String),
	/// Not found.
	#[error("plugin: not found")]
	NotFound,
	/// Allocation failed.
	#[error("plugin: out of memory")]
	NoMem,
}

impl Error {
	/// Maps to the error code in `include/plugin/error.h`.
	pub fn code(&self) -> i32 {
		match self {
			Error::Invalid => QUERCUSPLUGIN_E_INVALID,
			Error::State => QUERCUSPLUGIN_E_STATE,
			Error::Failed(_) => QUERCUSPLUGIN_E_FAILED,
			Error::NotFound => QUERCUSPLUGIN_E_NOT_FOUND,
			Error::NoMem => QUERCUSPLUGIN_E_NOMEM,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// One instance of every variant (data-carrying ones get a sample
	/// payload).
	fn all_errors() -> Vec<Error> {
		vec![
			Error::Invalid,
			Error::State,
			Error::Failed("boom".to_string()),
			Error::NotFound,
			Error::NoMem,
		]
	}

	#[test]
	fn display_is_non_empty_for_every_variant() {
		for e in all_errors() {
			let s = e.to_string();
			assert!(!s.is_empty(), "Display produced an empty message for {e:?}");
		}
	}

	#[test]
	fn error_is_object_safe() {
		// `Box<dyn std::error::Error>` must be constructible for every
		// variant; `source()` stays None (no wrapped downstream error).
		let errors: Vec<Box<dyn std::error::Error>> = all_errors()
			.into_iter()
			.map(|e| Box::new(e) as Box<dyn std::error::Error>)
			.collect();
		for e in &errors {
			assert!(!e.to_string().is_empty());
			assert!(e.source().is_none());
		}
	}

	#[test]
	fn code_is_unaffected_by_trait_impl() {
		assert_eq!(Error::Invalid.code(), QUERCUSPLUGIN_E_INVALID);
		assert_eq!(Error::State.code(), QUERCUSPLUGIN_E_STATE);
		assert_eq!(Error::Failed("boom".to_string()).code(), QUERCUSPLUGIN_E_FAILED);
		assert_eq!(Error::NotFound.code(), QUERCUSPLUGIN_E_NOT_FOUND);
		assert_eq!(Error::NoMem.code(), QUERCUSPLUGIN_E_NOMEM);
	}
}
