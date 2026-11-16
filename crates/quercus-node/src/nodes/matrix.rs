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

//! Orthographic matrix generator (C++
//! `src/node/src/generator/matrix/matrix.{h,cpp}`,
//! `olive::MatrixGenerator`).

use super::mathbase;
use crate::factory::NodeMeta;
use crate::input;
use crate::node::{Category, NodeBehavior, NodeCore};
use std::f64;

/// Position input id (C++ `k_position_input`). Type: vec2; default
/// `(0.0, 0.0)`.
pub const POSITION_INPUT: &str = "pos_in";

/// Rotation input id (C++ `k_rotation_input`). Type: float; default
/// `0.0`.
pub const ROTATION_INPUT: &str = "rot_in";

/// Scale input id (C++ `k_scale_input`). Type: vec2; default
/// `(1.0, 1.0)`; properties: `min = (0, 0)`, `view = percentage`,
/// `disable1 = true` (the second component starts disabled because
/// uniform scale defaults to on).
pub const SCALE_INPUT: &str = "scale_in";

/// Uniform scale input id (C++ `k_uniform_scale_input`). Type: bool;
/// default `true`; flags: not-connectable, not-keyframable.
pub const UNIFORM_SCALE_INPUT: &str = "uniform_scale_in";

/// Anchor point input id (C++ `k_anchor_input`). Type: vec2; default
/// `(0.0, 0.0)`.
pub const ANCHOR_INPUT: &str = "anchor_in";

/// Orthographic matrix generator node. Builds a 2D transform matrix
/// from position, rotation, scale and anchor inputs. Has no own member
/// fields in C++ (state lives in the `Node` inputs).
pub struct MatrixGenerator;

impl NodeBehavior for MatrixGenerator {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Orthographic Matrix"
	}

	/// Short menu name (C++ `short_name()`).
	fn short_name(&self) -> &str {
		"Ortho"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.ortho"
	}

	/// Categories (C++ `category()`): generator and math.
	fn categories(&self) -> &[Category] {
		&[Category::Generator, Category::Math]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Generate an orthographic matrix using position, rotation, and scale."
	}

	/// Localized input names (C++ `retranslate()`): `pos_in` ->
	/// "Position", `rot_in` -> "Rotation", `scale_in` -> "Scale",
	/// `uniform_scale_in` -> "Uniform Scale", `anchor_in` ->
	/// "Anchor Point".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			POSITION_INPUT => "Position",
			ROTATION_INPUT => "Rotation",
			SCALE_INPUT => "Scale",
			UNIFORM_SCALE_INPUT => "Uniform Scale",
			ANCHOR_INPUT => "Anchor Point",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): builds the matrix via
	/// `generate_matrix(value, false, false, false, Matrix4x4())` and
	/// pushes it as a `k_matrix` value. The C++ helper composes, in
	/// order: translate(position), rotate(rotation around Z),
	/// scale(scale.x, uniform ? scale.x : scale.y, 1), then
	/// translate(-anchor).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: quercus_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let mat = MatrixGenerator::generate_matrix(
			inputs,
			core,
			time,
			false,
			false,
			false,
			mathbase::identity_matrix(),
		);
		table.push(
			crate::value::ValueType::Matrix,
			crate::value::NodeValue::Matrix(mat),
			None,
		);
	}

	/// Input value changed (C++ `InputValueChangedEvent`): when
	/// `uniform_scale_in` changes, sets the scale input's `disable1`
	/// property to the uniform-scale value (disabling the Y component
	/// while uniform scale is on).
	fn input_value_changed(&mut self, core: &mut NodeCore, input: &str, element: i32) {
		if input == UNIFORM_SCALE_INPUT && element == -1 {
			let uniform = core.standard_value(UNIFORM_SCALE_INPUT, -1).to_double() != 0.0;
			if let Some(scale) = core.get_input_mut(SCALE_INPUT) {
				scale.properties.retain(|(k, _)| k != "disable1");
				scale.properties.push((
					"disable1".to_string(),
					crate::value::NodeValue::Boolean(uniform),
				));
			}
		}
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(MatrixGenerator))
	}
}

impl MatrixGenerator {
	/// C++ `generate_matrix(const NodeValueRow&, bool, bool, bool,
	/// const Matrix4x4&)` — reads the transform inputs from the render
	/// row (falling back to the keyframed/standard values via `core`),
	/// honoring the ignore flags, and composes the result onto `mat`
	/// (used by [`MatrixGenerator::value`] and `TransformDistortNode`).
	pub fn generate_matrix(
		row: &crate::value::NodeValueRow,
		core: &NodeCore,
		time: quercus_core::Rational,
		ignore_anchor: bool,
		ignore_position: bool,
		ignore_scale: bool,
		mat: [f64; 16],
	) -> [f64; 16] {
		let anchor = if ignore_anchor {
			[0.0, 0.0]
		} else {
			row_vec2(row, core, time, ANCHOR_INPUT)
		};
		let scale = if ignore_scale {
			[1.0, 1.0]
		} else {
			row_vec2(row, core, time, SCALE_INPUT)
		};
		let position = if ignore_position {
			[0.0, 0.0]
		} else {
			row_vec2(row, core, time, POSITION_INPUT)
		};
		let rotation = row_float(row, core, time, ROTATION_INPUT);
		let uniform_scale = row_bool(row, core, time, UNIFORM_SCALE_INPUT);

		Self::compose_matrix(position, rotation, scale, uniform_scale, anchor, mat)
	}

	/// Static matrix composition (C++
	/// `generate_matrix(const Vector2D&, const float&, const Vector2D&,
	/// bool, const Vector2D&, Matrix4x4)`): `mat` post-multiplied by
	/// translate(position), rotate(rotation degrees around Z),
	/// scale(scale.x, uniform ? scale.x : scale.y, 1), then
	/// translate(-anchor). Row-major 16-element storage; every
	/// post-multiply matches the C++ `Matrix4x4` member operations
	/// (`// CPP-PARITY: matrix.cpp` `generate_matrix`).
	pub fn compose_matrix(
		pos: [f64; 2],
		rot: f64,
		scale: [f64; 2],
		uniform_scale: bool,
		anchor: [f64; 2],
		mat: [f64; 16],
	) -> [f64; 16] {
		// Position
		let mut m = matrix_translate(mat, pos[0], pos[1]);

		// Rotation (2D rotation around the Z axis).
		m = matrix_rotate_z(m, rot);

		// Scale (uniform scale replicates the X component).
		let full_scale = if uniform_scale {
			(scale[0], scale[0])
		} else {
			(scale[0], scale[1])
		};
		m = matrix_scale(m, full_scale.0, full_scale.1, 1.0);

		// Anchor point
		matrix_translate(m, -anchor[0], -anchor[1])
	}
}

/// Multiplies two row-major 4x4 matrices (`out = a * b`), i.e.
/// post-multiplies `a`'s transform by `b`'s, matching C++
/// `Matrix4x4::operator*`.
///
/// Indexing is `m[r*4 + c]` as used throughout this crate, with the
/// translation column at indices `3`, `7`, `11`. With column-vector
/// convention `b` is applied first, so `matrix_mul(translate, scale)`
/// scales before translating.
///
/// # Arguments
///
/// * `a` - left operand, row-major 4x4.
/// * `b` - right operand, row-major 4x4.
///
/// # Returns
///
/// The product `a * b` (row-major 4x4).
pub fn matrix_mul(a: [f64; 16], b: [f64; 16]) -> [f64; 16] {
	let mut out = [0.0f64; 16];
	for r in 0..4 {
		for c in 0..4 {
			let mut acc = 0.0;
			for k in 0..4 {
				acc += a[r * 4 + k] * b[k * 4 + c];
			}
			out[r * 4 + c] = acc;
		}
	}
	out
}

/// Post-multiply a 2D translation (C++ `Matrix4x4::translate(x, y)`).
pub fn matrix_translate(m: [f64; 16], x: f64, y: f64) -> [f64; 16] {
	let mut t = mathbase::identity_matrix();
	t[3] = x;
	t[7] = y;
	matrix_mul(m, t)
}

/// Returns `m * S`, where `S` is the diagonal scale matrix
/// `diag(x, y, z, 1)` in row-major `m[r*4 + c]` layout.
///
/// With column vectors this scales inside `m`'s local space before `m`'s
/// own rotation/translation, so order relative to
/// [`matrix_translate`] matters. A zero `x`/`y` makes an affine
/// transform singular (see [`matrix_invert_2d`]).
///
/// # Arguments
///
/// * `m` - existing 4x4 matrix, row-major.
/// * `x` - scale factor along the local X axis.
/// * `y` - scale factor along the local Y axis.
/// * `z` - scale factor along the local Z axis.
pub fn matrix_scale(m: [f64; 16], x: f64, y: f64, z: f64) -> [f64; 16] {
	let mut s = mathbase::identity_matrix();
	s[0] = x;
	s[5] = y;
	s[10] = z;
	matrix_mul(m, s)
}

/// Post-multiply a Z-axis rotation in degrees (C++
/// `Matrix4x4::rotate(degrees)`).
fn matrix_rotate_z(m: [f64; 16], degrees: f64) -> [f64; 16] {
	let radians = degrees * f64::consts::PI / 180.0;
	let (c, s) = (radians.cos(), radians.sin());
	let mut r = mathbase::identity_matrix();
	r[0] = c;
	r[1] = -s;
	r[4] = s;
	r[5] = c;
	matrix_mul(m, r)
}

/// Invert a 2D affine transform (C++ `Matrix4x4::inverted` on the 2D
/// path). The matrices this crate generates are affine —
/// `[a b 0 tx; c d 0 ty; 0 0 sz 0; 0 0 0 1]` in the row-major
/// `m[r*4+c]` layout — so the inverse is analytic. `None` when the
/// affine part is singular (a zero scale collapses the plane; the C++
/// callers fall back to identity there).
pub fn matrix_invert_2d(m: [f64; 16]) -> Option<[f64; 16]> {
	let (a, b, tx) = (m[0], m[1], m[3]);
	let (c, d, ty) = (m[4], m[5], m[7]);
	let det = a * d - b * c;
	if det.abs() < 1e-12 {
		return None;
	}
	let inv = 1.0 / det;
	let (ia, ib, ic, id) = (d * inv, -b * inv, -c * inv, a * inv);
	let mut out = mathbase::identity_matrix();
	out[0] = ia;
	out[1] = ib;
	out[3] = -(ia * tx + ib * ty);
	out[4] = ic;
	out[5] = id;
	out[7] = -(ic * tx + id * ty);
	let sz = m[10];
	if sz.abs() > 1e-12 {
		out[10] = 1.0 / sz;
	}
	Some(out)
}

/// Resolve a vec2 input from the render row or the keyframed/standard
/// value (C++ `value.at(id).to_vec2()`; missing values read as
/// `(0, 0)`).
fn row_vec2(
	row: &crate::value::NodeValueRow,
	core: &NodeCore,
	time: quercus_core::Rational,
	id: &str,
) -> [f64; 2] {
	match row.get(id) {
		Some(crate::value::NodeValue::Vec2(v)) => *v,
		Some(v) => [v.to_double(), 0.0],
		None => match core.value_at_time(id, -1, time) {
			crate::value::NodeValue::Vec2(v) => v,
			v => [v.to_double(), 0.0],
		},
	}
}

/// Resolve a float input from the render row or the keyframed/standard
/// value (C++ `value.at(id).to_double()`).
fn row_float(
	row: &crate::value::NodeValueRow,
	core: &NodeCore,
	time: quercus_core::Rational,
	id: &str,
) -> f64 {
	match row.get(id) {
		Some(v) => v.to_double(),
		None => core.value_at_time(id, -1, time).to_double(),
	}
}

/// Resolve a boolean input from the render row or the keyframed/standard
/// value (C++ `value.at(id).to_bool()`).
fn row_bool(
	row: &crate::value::NodeValueRow,
	core: &NodeCore,
	time: quercus_core::Rational,
	id: &str,
) -> bool {
	match row.get(id) {
		Some(v) => v.to_double() != 0.0,
		None => core.value_at_time(id, -1, time).to_double() != 0.0,
	}
}

/// Constructor (C++ `MatrixGenerator::MatrixGenerator()`): adds
/// `pos_in`, `rot_in`, `scale_in`, `uniform_scale_in` and `anchor_in`
/// with the defaults, flags and properties documented on the constants.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut pos = input::Input::new(
		POSITION_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	);
	pos.properties = vec![(
		"view".to_string(),
		crate::value::NodeValue::Text("percentage".into()),
	)];
	core.add_input(pos);

	let mut rot = input::Input::new(
		ROTATION_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	rot.properties = vec![
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
		("min".to_string(), crate::value::NodeValue::Float(-360.0)),
		("max".to_string(), crate::value::NodeValue::Float(360.0)),
	];
	core.add_input(rot);

	let mut scale = input::Input::new(
		SCALE_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([1.0, 1.0]),
	);
	scale.properties = vec![
		("min".to_string(), crate::value::NodeValue::Vec2([0.0, 0.0])),
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
		(
			"disable1".to_string(),
			crate::value::NodeValue::Boolean(true),
		),
	];
	core.add_input(scale);

	let mut uniform = input::Input::new(
		UNIFORM_SCALE_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(true),
	);
	uniform.flags |= input::flags::NOT_CONNECTABLE | input::flags::NOT_KEYFRAMABLE;
	core.add_input(uniform);

	let mut anchor = input::Input::new(
		ANCHOR_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	);
	anchor.properties = vec![(
		"view".to_string(),
		crate::value::NodeValue::Text("percentage".into()),
	)];
	core.add_input(anchor);

	(core, Box::new(MatrixGenerator))
}

/// Registers the orthographic-matrix generator
/// (`org.olivevideoeditor.Olive.ortho`, shown as "Orthographic Matrix")
/// in the node factory.
///
/// The factory entry's `create` builds a transform node whose inputs are
/// Position, Rotation, Scale, Uniform Scale and Anchor Point and whose
/// output is the composed 4x4 matrix. Call once during node
/// registration; the function does not deduplicate.
///
/// # Arguments
///
/// * `meta` - factory metadata registry to append the entry to.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.ortho",
		name: "Orthographic Matrix",
		categories: &[Category::Generator, Category::Math],
		create,
	});
}

#[cfg(test)]
mod tests;
