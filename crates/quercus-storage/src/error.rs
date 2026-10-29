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

//! Error and info codes (M10 §2.1; -MMCCCC scheme, module 10).

use thiserror::Error;

/// Success.
pub const QUERCUSSTORAGE_OK: i32 = 0;
/// Project version too old (info code, positive).
pub const QUERCUSSTORAGE_TOO_OLD: i32 = 1;
/// Project version too new (info code).
pub const QUERCUSSTORAGE_TOO_NEW: i32 = 2;
/// Unknown project version (info code).
pub const QUERCUSSTORAGE_UNKNOWN_VERSION: i32 = 3;
/// Null handle or invalid argument.
pub const QUERCUSSTORAGE_E_INVALID: i32 = -100001;
/// Call not valid in the current state.
pub const QUERCUSSTORAGE_E_STATE: i32 = -100002;
/// Entry not found.
pub const QUERCUSSTORAGE_E_NOT_FOUND: i32 = -100003;
/// The underlying operation failed.
pub const QUERCUSSTORAGE_E_FAILED: i32 = -100004;
/// No backend claimed the URI.
pub const QUERCUSSTORAGE_E_NO_BACKEND: i32 = -100005;
/// Parse/format failure (XML/DB constraint).
pub const QUERCUSSTORAGE_E_FORMAT: i32 = -100006;
/// I/O failure.
pub const QUERCUSSTORAGE_E_IO: i32 = -100007;
/// Allocation failed.
pub const QUERCUSSTORAGE_E_NOMEM: i32 = -100008;

/// Crate-internal result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Crate-internal error.
#[derive(Debug, Error)]
pub enum Error {
	/// Null handle or invalid argument.
	#[error("storage: invalid argument")]
	Invalid,
	/// Wrong state.
	#[error("storage: call not valid in the current state")]
	State,
	/// Not found.
	#[error("storage: not found")]
	NotFound,
	/// Operation failed (context string is log-only).
	#[error("storage: operation failed: {0}")]
	Failed(String),
	/// No backend claimed the URI.
	#[error("storage: no backend claimed the URI")]
	NoBackend,
	/// Format error.
	#[error("storage: format error: {0}")]
	Format(String),
	/// I/O error.
	#[error("storage: I/O error: {0}")]
	Io(String),
	/// Out of memory.
	#[error("storage: out of memory")]
	NoMem,
}

impl Error {
	/// Map to the public error code.
	pub fn code(&self) -> i32 {
		match self {
			Error::Invalid => QUERCUSSTORAGE_E_INVALID,
			Error::State => QUERCUSSTORAGE_E_STATE,
			Error::NotFound => QUERCUSSTORAGE_E_NOT_FOUND,
			Error::Failed(_) => QUERCUSSTORAGE_E_FAILED,
			Error::NoBackend => QUERCUSSTORAGE_E_NO_BACKEND,
			Error::Format(_) => QUERCUSSTORAGE_E_FORMAT,
			Error::Io(_) => QUERCUSSTORAGE_E_IO,
			Error::NoMem => QUERCUSSTORAGE_E_NOMEM,
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
			Error::NotFound,
			Error::Failed("boom".to_string()),
			Error::NoBackend,
			Error::Format("bad xml".to_string()),
			Error::Io("disk full".to_string()),
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
		assert_eq!(Error::Invalid.code(), QUERCUSSTORAGE_E_INVALID);
		assert_eq!(Error::State.code(), QUERCUSSTORAGE_E_STATE);
		assert_eq!(Error::NotFound.code(), QUERCUSSTORAGE_E_NOT_FOUND);
		assert_eq!(Error::Failed("boom".to_string()).code(), QUERCUSSTORAGE_E_FAILED);
		assert_eq!(Error::NoBackend.code(), QUERCUSSTORAGE_E_NO_BACKEND);
		assert_eq!(Error::Format("bad xml".to_string()).code(), QUERCUSSTORAGE_E_FORMAT);
		assert_eq!(Error::Io("disk full".to_string()).code(), QUERCUSSTORAGE_E_IO);
		assert_eq!(Error::NoMem.code(), QUERCUSSTORAGE_E_NOMEM);
	}
}
