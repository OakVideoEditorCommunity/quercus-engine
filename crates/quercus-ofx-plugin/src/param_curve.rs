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

//! Parametric curve model (host-side data and evaluation for OfxParametricParameterSuite).
//!
//! A curve = a list of control points sorted by ascending key, each carrying a
//! Hermite tangent (slope). The suite ([`crate::suites::parametric`]) exposes only
//! key/value edit entry points, and slopes are always computed by
//! [`Curve::recompute_slopes`] — interior points use a centered finite
//! difference, endpoints a one-sided difference; fields stay `pub` so tests and
//! the host can edit slopes explicitly.
//!
//! Evaluation = piecewise cubic Hermite (see [`Curve::evaluate`]). This is the
//! "curve-editor style" representation the OFX spec recommends hosts use (the
//! parametric parameter documentation in ofxParametricParam.h); it is equivalent
//! to the Bezier form (a Hermite tangent × segment length is the Bezier control
//! arm).

/// A curve control point: `key` (parametric position, its domain bounded by
/// `OfxParamPropParametricRange`), `value` (the evaluation result), and
/// `slope` (the first derivative at that point, the cubic Hermite tangent).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlPoint {
	/// Parametric position (x).
	pub key: f64,
	/// Evaluation result (y).
	pub value: f64,
	/// First derivative (auto-differentiated / explicitly edited).
	pub slope: f64,
}

impl ControlPoint {
	/// Builds a control point from key/value; `slope` is initialized to 0.0.
	///
	/// `key` is the parametric position (x, its domain bounded by
	/// `OfxParamPropParametricRange`) and `value` is the evaluation result (y).
	/// The slope is a placeholder only: after putting the point into a [`Curve`] you
	/// must call [`Curve::recompute_slopes`] (or use [`Curve::upsert`] /
	/// [`Curve::from_pairs`], which recompute internally), otherwise Hermite
	/// evaluation treats the point as zero-tangent and the curve flattens at the
	/// control point.
	pub fn new(key: f64, value: f64) -> Self {
		Self {
			key,
			value,
			slope: 0.0,
		}
	}
}

/// A parametric curve: control points sorted by ascending key (an invariant
/// maintained by this module's mutation API).
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
	/// Control-point sequence (ascending keys, no duplicate keys).
	pub points: Vec<ControlPoint>,
}

impl Curve {
	/// An empty curve (the state after `DeleteAllControlPoints`; evaluation
	/// degenerates to identity, see [`Curve::evaluate`]).
	pub fn empty() -> Self {
		Self {
			points: Vec::new(),
		}
	}

	/// Identity curve: `{(lo, lo), (hi, hi)}` + automatic slopes — the default
	/// default of a parametric parameter (ofxParametricParam.h: "The default default
	/// value of a parametric curve is to be an identity lookup").
	/// `lo`/`hi` come from `OfxParamPropParametricRange` (default (0,1)).
	pub fn identity(lo: f64, hi: f64) -> Self {
		let mut c = Self {
			points: vec![
				ControlPoint::new(lo, lo),
				ControlPoint::new(hi, hi),
			],
		};
		c.recompute_slopes();
		c
	}

	/// Builds from key/value pairs (slopes auto-differentiated; in the given order —
	/// the caller guarantees ascending, or inserts point by point via [`Curve::upsert`]).
	pub fn from_pairs(pairs: &[(f64, f64)]) -> Self {
		let mut c = Self {
			points: pairs
				.iter()
				.map(|&(k, v)| ControlPoint::new(k, v))
				.collect(),
		};
		c.recompute_slopes();
		c
	}

	/// Number of control points.
	pub fn len(&self) -> usize {
		self.points.len()
	}

	/// Whether the curve is empty (after `DeleteAllControlPoints`).
	pub fn is_empty(&self) -> bool {
		self.points.is_empty()
	}

	/// Returns a read-only reference to the `i`-th control point (0-based, ascending key).
	///
	/// `i` out of range returns `None`; shares the same index space as
	/// [`Curve::set_nth`] / [`Curve::delete_nth`]. Read-only access leaves slopes
	/// untouched; use [`Curve::upsert`] / [`Curve::set_nth`] to modify.
	pub fn nth(&self, i: usize) -> Option<&ControlPoint> {
		self.points.get(i)
	}

	/// Recomputes all slopes: centered difference for interior points (span on both
	/// sides), one-sided at the endpoints; single-point/empty curves have no
	/// difference → slope 0. Call it after changing key/value, otherwise the
	/// Hermite tangents lag behind the control-point geometry.
	pub fn recompute_slopes(&mut self) {
		let n = self.points.len();
		for i in 0..n {
			self.points[i].slope = slope_at(&self.points, i);
		}
	}

	/// Cubic Hermite evaluation.
	///
	/// - Empty curve: identity (f(x) = x) — consistent with "the default default is
	///   an identity lookup"; after `DeleteAllControlPoints` the curve degenerates to
	///   a neutral identity with no endpoint to clamp, so x is returned directly;
	/// - Single-point curve: constant (that point's value);
	/// - x outside the first/last key: clamped to the endpoint value (task/spec:
	///   out-of-range keys clamp to the endpoint value).
	///
	/// Math: on the segment [x0, x1] let t = (x - x0)/(x1 - x0), h = x1 - x0, with
	/// cubic Hermite basis functions
	///   h00 = 2t³ - 3t² + 1,  h01 = -2t³ + 3t²
	///   h10 = t³ - 2t² + t,   h11 = t³ - t²
	/// f(x) = h00·y0 + h01·y1 + h·(h10·m0 + h11·m1).
	/// An identity curve with slope 1 degenerates exactly to f(x) = x (outside endpoint clamping).
	pub fn evaluate(&self, x: f64) -> f64 {
		let n = self.points.len();
		if n == 0 {
			return x;
		}
		if n == 1 {
			return self.points[0].value;
		}
		if x <= self.points[0].key {
			return self.points[0].value;
		}
		if x >= self.points[n - 1].key {
			return self.points[n - 1].value;
		}
		// 定位所在段：points[i].key <= x < points[i+1].key。
		let i = self.points.partition_point(|p| p.key <= x) - 1;
		let a = self.points[i];
		let b = self.points[i + 1];
		let h = b.key - a.key;
		if h <= 0.0 {
			return a.value; // 防御：重复 key（不变量保证不出现）
		}
		let t = (x - a.key) / h;
		let t2 = t * t;
		let t3 = t2 * t;
		let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
		let h01 = -2.0 * t3 + 3.0 * t2;
		let h10 = t3 - 2.0 * t2 + t;
		let h11 = t3 - t2;
		h00 * a.value + h01 * b.value + h * (h10 * a.slope + h11 * b.slope)
	}

	/// Inserts (key, value): an existing key has its value overwritten (returns false);
	/// otherwise the point is inserted in ascending key order (returns true). Slopes
	/// are recomputed afterwards. This is the common landing point for Add and
	/// SetNth (SetNth removes the point first, then lands here).
	pub fn upsert(&mut self, key: f64, value: f64) -> bool {
		if let Some(p) = self.points.iter_mut().find(|p| p.key == key) {
			p.value = value;
			self.recompute_slopes();
			return false;
		}
		let idx = self.points.partition_point(|p| p.key < key);
		self.points.insert(idx, ControlPoint::new(key, value));
		self.recompute_slopes();
		true
	}

	/// Changes the `nth` control point to (key, value). A key change can break the
	/// ordering (ofxParametricParam.h: SetNthControlPoint's key may move before/after
	/// other points) — remove first, then insert by key; when the new key collides
	/// with another point's key, overwrite semantics apply. nth out of range → Err(()).
	pub fn set_nth(&mut self, nth: usize, key: f64, value: f64) -> Result<(), ()> {
		if nth >= self.points.len() {
			return Err(());
		}
		self.points.remove(nth);
		self.upsert(key, value);
		Ok(())
	}

	/// Deletes the `nth` control point, then recomputes all slopes.
	///
	/// `nth` is a 0-based index in ascending key order; out of range returns `Err(())`
	/// and leaves the curve unchanged. Shares the same index space as
	/// [`Curve::set_nth`] / [`Curve::upsert`]; use [`Curve::clear`] to drop every
	/// control point (`DeleteAllControlPoints`). The deleted point is not returned.
	pub fn delete_nth(&mut self, nth: usize) -> Result<(), ()> {
		if nth >= self.points.len() {
			return Err(());
		}
		self.points.remove(nth);
		self.recompute_slopes();
		Ok(())
	}

	/// Deletes every control point (`DeleteAllControlPoints`).
	pub fn clear(&mut self) {
		self.points.clear();
	}
}

// ---- 曲线集 ↔ JSON（节点输入 / 工程序列化的值载荷）--------------------
//
// 格式（确定性、紧凑，无空白）：
//   {"curves":[[{"key":0.0,"value":0.0,"slope":1.0},...],...]}
// 外层按 dimension 顺序一维一条曲线；每维一个控制点对象，字段序固定
// key/value/slope。数值用 Rust 最短往返格式化（`{}`）。非有限值 JSON
// 无标准写法，取对称 token：NaN → `null`，+Inf → `"inf"`，
// -Inf → `"-inf"`（字符串字面量；解析器对称处理，有限值恒为裸数字）。
// 解析器容忍空白与字段乱序；结构不符 → None（调用方按字符串族静默
// 路径处理）。手写实现，不引入 serde（crate 无该依赖）。

/// Serializes a curve set to compact JSON (no whitespace) as a node input default
/// value and as the payload a plugin writes back to a node input.
///
/// The outer `{"curves":[...]}` holds one curve per slice entry, in slice order
/// (i.e. dimension order); each control-point object has the fixed field order
/// `key`/`value`/`slope` and numbers use the shortest round-trip format (see
/// [`push_num`]; NaN/±Inf get dedicated tokens). The inverse of
/// [`curves_from_json`]; the output contains no newlines or spaces.
pub fn curves_to_json(curves: &[Curve]) -> String {
	let mut out =
		String::with_capacity(8 + 12 * curves.iter().map(|c| c.len()).sum::<usize>());
	out.push_str("{\"curves\":[");
	for (ci, c) in curves.iter().enumerate() {
		if ci > 0 {
			out.push(',');
		}
		out.push('[');
		for (pi, p) in c.points.iter().enumerate() {
			if pi > 0 {
				out.push(',');
			}
			out.push_str("{\"key\":");
			push_num(&mut out, p.key);
			out.push_str(",\"value\":");
			push_num(&mut out, p.value);
			out.push_str(",\"slope\":");
			push_num(&mut out, p.slope);
			out.push('}');
		}
		out.push(']');
	}
	out.push_str("]}");
	out
}

/// Parses the JSON produced by [`curves_to_json`] back into a curve set; returns
/// `None` when the structure does not match.
///
/// The parser tolerates whitespace and out-of-order fields; a control point with no
/// `slope` is filled with 0.0. Points keep the file's original order and slopes are
/// preserved as-is (not recomputed), so the round trip with [`curves_to_json`] is
/// bit-exact. A non-object root, a key other than `curves`, broken array/object
/// syntax, unknown fields, a missing `key`/`value` or trailing bytes all return
/// `None`, handled by the caller on the string family's silent-failure path.
pub fn curves_from_json(text: &str) -> Option<Vec<Curve>> {
	let mut p = JsonParser {
		bytes: text.as_bytes(),
		pos: 0,
	};
	p.skip_ws();
	if !p.eat(b'{') {
		return None;
	}
	p.skip_ws();
	if p.parse_string()?.as_str() != "curves" {
		return None;
	}
	p.skip_ws();
	if !p.eat(b':') {
		return None;
	}
	p.skip_ws();
	let mut curves = Vec::new();
	if p.eat(b'[') {
		loop {
			p.skip_ws();
			if p.eat(b']') {
				break;
			}
			curves.push(p.parse_curve()?);
			p.skip_ws();
			if p.eat(b',') {
				continue;
			}
			if p.eat(b']') {
				break;
			}
			return None;
		}
	} else {
		return None;
	}
	p.skip_ws();
	if !p.eat(b'}') {
		return None;
	}
	p.skip_ws();
	if p.pos != p.bytes.len() {
		return None;
	}
	Some(curves)
}

/// Appends the JSON token of a single number to `out` (no commas or structural characters).
///
/// Finite values are written as the shortest round-trip decimal of `{}`; non-finite
/// values use symmetric tokens JSON has no standard spelling for: NaN → `null`,
/// +Inf → `"inf"`, -Inf → `"-inf"` (string literals), matching the parsing rules
/// of [`JsonParser::parse_num`] one-to-one.
fn push_num(out: &mut String, v: f64) {
	if v.is_nan() {
		out.push_str("null");
	} else if v == f64::INFINITY {
		out.push_str("\"inf\"");
	} else if v == f64::NEG_INFINITY {
		out.push_str("\"-inf\"");
	} else {
		out.push_str(&format!("{v}"));
	}
}

/// Hand-written JSON parser (only the subset this format needs; field names carry
/// no escaped quotes and control-point fields are fixed identifiers).
struct JsonParser<'a> {
	bytes: &'a [u8],
	pos: usize,
}

impl<'a> JsonParser<'a> {
	fn skip_ws(&mut self) {
		while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
			self.pos += 1;
		}
	}

	fn eat(&mut self, byte: u8) -> bool {
		if self.bytes.get(self.pos) == Some(&byte) {
			self.pos += 1;
			true
		} else {
			false
		}
	}

	/// If the current position starts with the byte sequence `want`, advances by
	/// `want.len()` and returns `true`; otherwise leaves `pos` untouched and returns `false`.
	///
	/// Pure byte-prefix matching, with no string semantics (it neither requires `want`
	/// to be valid UTF-8 nor skips whitespace); used for field names and fixed tokens
	/// such as `null`. Callers must call [`JsonParser::skip_ws`] first and roll back
	/// failed matches themselves.
	fn eat_str(&mut self, want: &[u8]) -> bool {
		if self.bytes[self.pos..].starts_with(want) {
			self.pos += want.len();
			true
		} else {
			false
		}
	}

	/// Quoted string (no escapes; used for field names and the `"inf"` token).
	fn parse_string(&mut self) -> Option<String> {
		if !self.eat(b'"') {
			return None;
		}
		let start = self.pos;
		while self.pos < self.bytes.len() && self.bytes[self.pos] != b'"' {
			self.pos += 1;
		}
		let s = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?.to_string();
		if !self.eat(b'"') {
			return None;
		}
		Some(s)
	}

	/// Number token: `null` → NaN; `"inf"`/`"-inf"` → ±∞; otherwise a bare number
	/// (`f64::from_str`, overflow → ±∞, symmetric with the shortest `{}` round trip).
	fn parse_num(&mut self) -> Option<f64> {
		if self.eat_str(b"null") {
			return Some(f64::NAN);
		}
		if self.bytes.get(self.pos) == Some(&b'"') {
			return match self.parse_string()?.as_str() {
				"inf" => Some(f64::INFINITY),
				"-inf" => Some(f64::NEG_INFINITY),
				_ => None,
			};
		}
		let start = self.pos;
		while self.pos < self.bytes.len()
			&& matches!(
				self.bytes[self.pos],
				b'-' | b'+' | b'.' | b'0'..=b'9' | b'e' | b'E'
			)
		{
			self.pos += 1;
		}
		if self.pos == start {
			return None;
		}
		std::str::from_utf8(&self.bytes[start..self.pos])
			.ok()?
			.parse()
			.ok()
	}

	/// One curve: `[` control points* `]` (points stored in ascending key order; parsed in file order).
	fn parse_curve(&mut self) -> Option<Curve> {
		if !self.eat(b'[') {
			return None;
		}
		let mut points = Vec::new();
		loop {
			self.skip_ws();
			if self.eat(b']') {
				break;
			}
			points.push(self.parse_point()?);
			self.skip_ws();
			if self.eat(b',') {
				continue;
			}
			if self.eat(b']') {
				break;
			}
			return None;
		}
		Some(Curve { points })
	}

	/// Parses one control-point object: `{"key":number,"value":number,"slope":number}`.
	///
	/// Fields may be out of order or repeated (later occurrences override earlier
	/// ones); `slope` may be omitted (filled with 0.0). An unknown field name, a
	/// missing `key` or `value`, or bad syntax returns `None`. Number tokens go
	/// through [`JsonParser::parse_num`] (`null` → NaN, `"inf"`/`"-inf"` → ±∞,
	/// otherwise a bare number). Points are not sorted here; the order is preserved
	/// from the file by [`JsonParser::parse_curve`].
	fn parse_point(&mut self) -> Option<ControlPoint> {
		if !self.eat(b'{') {
			return None;
		}
		let mut key = None;
		let mut value = None;
		let mut slope = None;
		loop {
			self.skip_ws();
			if self.eat(b'}') {
				break;
			}
			let name = self.parse_string()?;
			self.skip_ws();
			if !self.eat(b':') {
				return None;
			}
			self.skip_ws();
			let v = self.parse_num()?;
			match name.as_str() {
				"key" => key = Some(v),
				"value" => value = Some(v),
				"slope" => slope = Some(v),
				_ => return None,
			}
			self.skip_ws();
			if self.eat(b',') {
				continue;
			}
			if self.eat(b'}') {
				break;
			}
			return None;
		}
		Some(ControlPoint {
			key: key?,
			value: value?,
			slope: slope.unwrap_or(0.0),
		})
	}
}

/// The finite-difference slope at point `i` (no recompute; used by [`Curve::recompute_slopes`]).
/// Interior points: centered difference (y_{i+1} - y_{i-1}) / (x_{i+1} - x_{i-1});
/// endpoints: one-sided difference; single-point/empty curve: 0. Duplicate keys are guarded against division by zero → 0.
fn slope_at(points: &[ControlPoint], i: usize) -> f64 {
	let n = points.len();
	if n < 2 {
		return 0.0;
	}
	let (a, b) = if i == 0 {
		(&points[0], &points[1])
	} else if i == n - 1 {
		(&points[n - 2], &points[n - 1])
	} else {
		(&points[i - 1], &points[i + 1])
	};
	let dx = b.key - a.key;
	if dx == 0.0 {
		return 0.0;
	}
	(b.value - a.value) / dx
}

#[cfg(test)]
mod tests {
	use super::*;

	fn close(a: f64, b: f64, eps: f64) -> bool {
		(a - b).abs() <= eps
	}

	/// Identity curve: {(0,0),(1,1)} + automatic slopes (1,1) → Hermite is exactly
	/// f(x) = x (exact mathematically, not bit-exact in floating point — compared
	/// approximately); a non-default range is equally identity.
	#[test]
	fn identity_curve_evaluates_to_x() {
		let c = Curve::identity(0.0, 1.0);
		assert_eq!(c.len(), 2);
		assert_eq!(
			c.points[0],
			ControlPoint {
				key: 0.0,
				value: 0.0,
				slope: 1.0
			}
		);
		assert_eq!(
			c.points[1],
			ControlPoint {
				key: 1.0,
				value: 1.0,
				slope: 1.0
			}
		);
		for x in [0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0] {
			assert!(close(c.evaluate(x), x, 1e-12), "identity at {x}");
		}
		// 非默认 range（插件改 OfxParamPropParametricRange 后，
		// 新曲线的默认 default 相应平移）。
		let c = Curve::identity(0.0, 255.0);
		assert!(close(c.evaluate(128.0), 128.0, 1e-12));
	}

	/// Single-segment parabola shape: {(0,0),(0.5,0.25),(1,1)} (y = x² sampled).
	/// Automatic slopes = (0.5, 1, 1.5) (one-sided at the endpoints, centered at the
	/// middle); evaluation equals the analytic values of the piecewise cubic Hermite
	/// (hits the control points, increasing monotonically on both sides, hand-computed midpoint).
	#[test]
	fn parabola_shape_honours_hermite() {
		let c = Curve::from_pairs(&[(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)]);
		assert_eq!(
			c.points.iter().map(|p| p.slope).collect::<Vec<_>>(),
			vec![0.5, 1.0, 1.5]
		);
		// 插值性质：控制点处精确命中。
		assert_eq!(c.evaluate(0.0), 0.0);
		assert_eq!(c.evaluate(0.5), 0.25);
		assert_eq!(c.evaluate(1.0), 1.0);
		// 中点解析值（t = 1/2 段内，精确二进制分数）。
		assert_eq!(c.evaluate(0.125), 0.05078125);
		assert_eq!(c.evaluate(0.25), 0.09375);
		assert_eq!(c.evaluate(0.75), 0.59375);
		// 形状：单调递增；首段在抛物线 y = x² 之上（Hermite 端点
		// 用弦斜率、中点用中心差分 → 首段整体上凸）。
		let xs = [0.1, 0.2, 0.3, 0.4, 0.6, 0.7, 0.8, 0.9];
		for w in xs.windows(2) {
			assert!(c.evaluate(w[0]) < c.evaluate(w[1]));
		}
		assert!(c.evaluate(0.25) > 0.25 * 0.25); // 高于抛物线采样点
	}

	/// Out-of-range clamping: x outside the first/last key → endpoint value.
	#[test]
	fn out_of_range_clamps_to_endpoints() {
		let c = Curve::from_pairs(&[(0.2, 0.3), (0.8, 0.9)]);
		assert_eq!(c.evaluate(-100.0), 0.3);
		assert_eq!(c.evaluate(0.19), 0.3);
		assert_eq!(c.evaluate(0.2), 0.3);
		assert_eq!(c.evaluate(0.81), 0.9);
		assert_eq!(c.evaluate(100.0), 0.9);
	}

	/// Evaluation stays correct after insert/delete: upserting a new point hits at
	/// that key and keeps the order; deleting restores the two-point shape; an
	/// overwrite with the same key adds no point.
	#[test]
	fn upsert_and_delete_keep_evaluation_consistent() {
		let mut c = Curve::from_pairs(&[(0.0, 0.0), (1.0, 1.0)]);
		// 插入中点（返回 true = 新增），求值在其 key 处精确命中。
		assert!(c.upsert(0.3, 0.7));
		assert_eq!(c.len(), 3);
		assert_eq!(c.evaluate(0.3), 0.7);
		// 有序性保持。
		let keys: Vec<f64> = c.points.iter().map(|p| p.key).collect();
		assert_eq!(keys, vec![0.0, 0.3, 1.0]);
		// 同 key 覆盖（返回 false = 覆盖），数量不变。
		assert!(!c.upsert(0.3, 0.5));
		assert_eq!(c.len(), 3);
		assert_eq!(c.evaluate(0.3), 0.5);
		// 删除 → 恢复双点（恒等；非 2 幂 key 处逐位有舍入）。
		c.delete_nth(1).unwrap();
		assert_eq!(c.len(), 2);
		assert!(close(c.evaluate(0.3), 0.3, 1e-12));
		// 越界删除 → Err。
		assert!(c.delete_nth(2).is_err());
	}

	/// set_nth keeps the order after a key change (the point moves within the sequence).
	#[test]
	fn set_nth_reorders_on_key_change() {
		let mut c = Curve::from_pairs(&[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]);
		// 把第 0 点挪到 0.75（原第 1、2 点之前……之后）。
		c.set_nth(0, 0.75, 0.75).unwrap();
		let pairs: Vec<(f64, f64)> = c
			.points
			.iter()
			.map(|p| (p.key, p.value))
			.collect();
		assert_eq!(pairs, vec![(0.5, 0.5), (0.75, 0.75), (1.0, 1.0)]);
		assert_eq!(c.evaluate(0.75), 0.75);
		// 越界 nth → Err。
		assert!(c.set_nth(3, 0.9, 0.9).is_err());
	}

	/// Slope editing takes effect: for the same {(0,0),(1,1)}, automatic slopes (1,1)
	/// make evaluation the identity; flattening both slopes to 0 turns the curve into
	/// an S shape that eases in and out (the midpoint value moves from 0.25 to 0.15625).
	#[test]
	fn slope_editing_changes_evaluation() {
		let mut c = Curve::from_pairs(&[(0.0, 0.0), (1.0, 1.0)]);
		assert_eq!(c.evaluate(0.25), 0.25);
		// 显式编辑 slope（宿主/测试路径；suite 无 slope 入口，
		// 但字段公开即契约的一部分）。
		c.points[0].slope = 0.0;
		c.points[1].slope = 0.0;
		assert_eq!(c.evaluate(0.25), 0.15625);
		assert_eq!(c.evaluate(0.5), 0.5);
		assert!(close(c.evaluate(0.25), 0.15625, 1e-12));
	}

	/// Empty and single-point curves: identity / constant degeneration.
	#[test]
	fn degenerate_curves() {
		let c = Curve::empty();
		assert!(c.is_empty());
		assert_eq!(c.evaluate(0.3), 0.3);
		assert_eq!(c.evaluate(5.0), 5.0);
		let c = Curve::from_pairs(&[(0.5, 0.25)]);
		assert_eq!(c.evaluate(0.0), 0.25);
		assert_eq!(c.evaluate(100.0), 0.25);
	}

	/// clear: the model-side semantics of DeleteAllControlPoints.
	#[test]
	fn clear_empties_curve() {
		let mut c = Curve::from_pairs(&[(0.0, 0.0), (1.0, 1.0)]);
		c.clear();
		assert!(c.is_empty());
		assert_eq!(c.evaluate(0.5), 0.5);
	}

	/// JSON round trip: identity curve (default value) serialization shape verbatim + parse back to an equal model.
	#[test]
	fn json_roundtrip_identity() {
		let curves = vec![Curve::identity(0.0, 1.0)];
		let json = curves_to_json(&curves);
		assert_eq!(
			json,
			r#"{"curves":[[{"key":0,"value":0,"slope":1},{"key":1,"value":1,"slope":1}]]}"#
		);
		let back = curves_from_json(&json).expect("应可解析");
		assert_eq!(back, curves);
	}

	/// JSON round trip: multiple dimensions + non-trivial shape + explicitly edited
	/// slopes (the slope is part of the payload too and is preserved bit-exactly).
	#[test]
	fn json_roundtrip_multidim_and_slope() {
		let mut c = Curve::from_pairs(&[(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)]);
		c.points[1].slope = 0.0; // 显式编辑（字段公开即契约）
		let curves = vec![c.clone(), Curve::identity(0.0, 255.0)];
		let json = curves_to_json(&curves);
		let back = curves_from_json(&json).expect("应可解析");
		assert_eq!(back.len(), 2);
		assert_eq!(back, curves);
		assert_eq!(back[0].points[1].slope, 0.0);
		assert_eq!(back[1].points[1].key, 255.0);
		// 求值不受序列化影响。
		assert_eq!(back[0].evaluate(0.5), 0.25);
	}

	/// JSON round trip: special values (NaN/±Inf, in slopes and values) and empty curve/set.
	#[test]
	fn json_roundtrip_special_and_empty() {
		let curves = vec![Curve {
			points: vec![
				ControlPoint {
					key: 0.0,
					value: f64::NAN,
					slope: 0.0,
				},
				ControlPoint {
					key: 1.0,
					value: f64::INFINITY,
					slope: f64::NEG_INFINITY,
				},
			],
		}];
		let json = curves_to_json(&curves);
		assert_eq!(
			json,
			r#"{"curves":[[{"key":0,"value":null,"slope":0},{"key":1,"value":"inf","slope":"-inf"}]]}"#
		);
		let back = curves_from_json(&json).expect("应可解析");
		assert!(back[0].points[0].value.is_nan());
		assert_eq!(back[0].points[1].value, f64::INFINITY);
		assert_eq!(back[0].points[1].slope, f64::NEG_INFINITY);

		// 空曲线（DeleteAll 后）与空集。
		let curves = vec![Curve::empty(), Curve::empty()];
		let json = curves_to_json(&curves);
		assert_eq!(json, r#"{"curves":[[],[]]}"#);
		assert_eq!(curves_from_json(&json).unwrap(), curves);
		assert_eq!(curves_from_json(r#"{"curves":[]}"#).unwrap(), Vec::<Curve>::new());
	}

	/// JSON parsing: tolerates whitespace and out-of-order fields; mismatch → None (silent path).
	#[test]
	fn json_parser_tolerances() {
		let curves = vec![Curve::from_pairs(&[(0.0, 0.0), (1.0, 1.0)])];
		// 空白 + 字段乱序。
		let spaced = r#"{ "curves" : [ [ { "slope" : 1 , "key" : 0 , "value" : 0 } , { "key" : 1 , "value" : 1 , "slope" : 1 } ] ] }"#;
		assert_eq!(curves_from_json(spaced).unwrap(), curves);
		// 结构不符 → None。
		for bad in [
			"",
			"{}",
			r#"{"curves"}"#,
			r#"{"curve":[]}"#,
			r#"{"curves":[{"key":0}]}"#,
			r#"{"curves":[[{"key":"x"}]]}"#,
			r#"{"curves":[[{"key":0}]]"#,
			r#"{"curves":[[{"key":0,"value":0,"slope":0}]]}junk"#,
		] {
			assert!(curves_from_json(bad).is_none(), "应拒绝：{bad}");
		}
		// 缺 slope 字段 → 0（宽容）。
		let no_slope = r#"{"curves":[[{"key":0,"value":0}]]}"#;
		let back = curves_from_json(no_slope).unwrap();
		assert_eq!(back[0].points[0].slope, 0.0);
	}
}
