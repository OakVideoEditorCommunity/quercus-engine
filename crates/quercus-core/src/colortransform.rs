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

//! `olive::ColorTransform` — a color transform description. Mirrors
//! `src/common/src/colortransform.h` and `include/common/colortransform.h`.
//!
//! A transform is either an output-colorspace transform or a
//! display/view/look transform. The C++ object is passed through the C ABI
//! as a refcounted handle ([`QuercusColorTransform`] in `crate::ffi`); this
//! module owns the plain-data description behind the handle.
//!
//! The C++-only functions `quercus_core_colortransform_init_from_native` /
//! `get_native` take or return `olive::ColorTransform` and cannot be
//! expressed from Rust; they are served by the C++ adapter layer, not here.

/// `olive::ColorTransform` — the plain-data description behind the handle.
pub struct ColorTransform {
	/// Output colorspace name (empty when this is a display transform).
	output: String,
	/// Display name (empty when this is an output-colorspace transform).
	display: String,
	/// Whether this is a display/view/look transform.
	// CPP-PARITY: the C++ class tracks this with an explicit `is_display_`
	// member (`src/common/src/colortransform.h`). We keep it as a separate
	// bool as well so an empty display name on a display transform is still
	// recognized as a display transform.
	is_display: bool,
	/// View name (display transforms only).
	view: String,
	/// Look name (display transforms only).
	look: String,
}

impl ColorTransform {
	/// Creates an output-colorspace transform.
	///
	/// `output` is the target colorspace name (copied verbatim; it may be
	/// empty, which still yields an output transform). The display, view and
	/// look fields start empty and [`Self::is_display`] is `false`.
	/// Counterpart of [`Self::new_display`]; read the name back with
	/// [`Self::output`].
	pub fn new_output(output: &str) -> Self {
		Self {
			output: output.to_string(),
			display: String::new(),
			is_display: false,
			view: String::new(),
			look: String::new(),
		}
	}

	/// Creates a display/view/look transform.
	///
	/// `display` is the display (monitor) colorspace name, `view` the view
	/// transform name within it, and `look` an optional look name (pass `""`
	/// for none). All three are copied verbatim and may be empty. The
	/// transform is marked as a display transform via the private
	/// `is_display` flag, so an empty `display` name still reports
	/// [`Self::is_display`] as `true`; the output colorspace stays empty.
	/// Counterpart of [`Self::new_output`]; paired with [`Self::display`],
	/// [`Self::view`] and [`Self::look`].
	pub fn new_display(display: &str, view: &str, look: &str) -> Self {
		// CPP-PARITY: the C++ display constructor stores the display name in
		// the shared `output_` member and sets `is_display_ = true`; here the
		// display name lives in its own `display` field (the skeleton's public
		// `display()` / `output()` accessors read the respective field).
		Self {
			output: String::new(),
			display: display.to_string(),
			is_display: true,
			view: view.to_string(),
			look: look.to_string(),
		}
	}

	/// Whether this is a display/view/look transform.
	pub fn is_display(&self) -> bool {
		self.is_display
	}

	/// Display name (empty if this is an output transform).
	pub fn display(&self) -> &str {
		&self.display
	}

	/// Output colorspace name (empty if this is a display transform).
	pub fn output(&self) -> &str {
		&self.output
	}

	/// The view name of a display/view/look transform (e.g. `"standard"`,
	/// `"ACES"`), borrowed from the transform.
	///
	/// Always empty for output-colorspace transforms (see
	/// [`Self::new_output`]). The value is stored verbatim at construction and
	/// is not validated against the display's available views.
	pub fn view(&self) -> &str {
		&self.view
	}

	/// The look name of a display/view/look transform (e.g. `"none"`,
	/// `"soft"`), borrowed from the transform.
	///
	/// Always empty for output-colorspace transforms (see
	/// [`Self::new_output`]) and for display transforms built without a look.
	/// The value is stored verbatim at construction; it is not validated
	/// against any list of available looks.
	pub fn look(&self) -> &str {
		&self.look
	}
}

#[cfg(test)]
mod tests {
	use super::ColorTransform;

	#[test]
	fn output_transform_defaults() {
		let t = ColorTransform::new_output("sRGB");
		assert!(!t.is_display());
		assert_eq!(t.output(), "sRGB");
		assert_eq!(t.display(), "");
		assert_eq!(t.view(), "");
		assert_eq!(t.look(), "");
	}

	#[test]
	fn display_transform_defaults() {
		let t = ColorTransform::new_display("DCI-P3", "standard", "soft");
		assert!(t.is_display());
		assert_eq!(t.display(), "DCI-P3");
		assert_eq!(t.view(), "standard");
		assert_eq!(t.look(), "soft");
		assert_eq!(t.output(), "");
	}

	#[test]
	fn output_transform_empty_strings() {
		// Mirrors the C++ default constructor: an output transform built from
		// an empty string still has `is_display == false`.
		let t = ColorTransform::new_output("");
		assert!(!t.is_display());
		assert_eq!(t.output(), "");
	}

	#[test]
	fn display_transform_recognized_even_with_empty_display_name() {
		// The explicit is_display flag (vs. deriving from a non-empty display
		// name) keeps an empty-named display transform identifiable.
		let t = ColorTransform::new_display("", "view", "");
		assert!(t.is_display());
		assert_eq!(t.display(), "");
		assert_eq!(t.view(), "view");
	}

	#[test]
	fn accessors_return_borrowed_slices() {
		let t = ColorTransform::new_display("Display", "View", "Look");
		let d: &str = t.display();
		let v: &str = t.view();
		let l: &str = t.look();
		let o: &str = t.output();
		assert_eq!(d, "Display");
		assert_eq!(v, "View");
		assert_eq!(l, "Look");
		assert_eq!(o, "");
	}

	#[test]
	fn c_api_string_sizes() {
		// The c_api string getters return the required size including the NUL
		// (value.size() + 1) and, being NON-TRUNCATING, only copy when the
		// buffer is large enough. Verify the values the domain layer exposes
		// line up with those sizes.
		let t = ColorTransform::new_display("RGB", "ACES", "none");
		assert_eq!(t.display().len() + 1, 4);
		assert_eq!(t.view().len() + 1, 5);
		assert_eq!(t.look().len() + 1, 5);
	}
}
