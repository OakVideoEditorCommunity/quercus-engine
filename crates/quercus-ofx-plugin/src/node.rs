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

//! quercusnode bridge (single-lib unification): node-value POD, identity
//! registry, and undoable parameter write-back.
//!
//! ## Frozen `Value` layout
//!
//! [`Value`] is the `quercusnode_value` POD from include/node/node.h:93, with
//! field-for-field identical layout (type/num/den/f[4]; the `type` values are
//! listed in [`node_value_type`]).
//! String-family inputs (k_file/k_text/k_font/k_str_combo, node.h:48-52) have
//! no POD representation — they go through the dedicated `*_input_string_*`
//! functions (this bridge's [`set_input_string_undoable`]); the
//! `QUERCUSNODE_VALUE_STRING` POD does not carry string data.
//!
//! ## Identity registry (rebuilt for the single library)
//!
//! quercusnode's C ABI has been removed (single-lib unification): a node
//! object is a `quercus_node::graph::Graph` entry inside
//! `quercus_node::project::Project` (Arc<Mutex<Project>>), addressed by
//! [`quercus_node::id::NodeId`]. This crate's [`NodeRef`] is a value-type
//! replica of the old handle box `(Arc<Mutex<Project>>, NodeId)`; the
//! process-wide registry [`register_node`]/[`node_from_identity`] keeps a weak
//! mapping keyed by the packed identity (u64) of
//! [`quercus_node::id::NodeId::identity`] (entries die once the project is
//! released, and a failed upgrade yields None) — the counterpart of the address
//! semantics of the M9 C++ `quercusnode_node_identity()` registry. During
//! facade assembly [`register_node`] registers the node, and the returned
//! identity is written into [`crate::instance::Instance::bind_node`]
//! (`node_identity`).
//!
//! ## Undoable write-back
//!
//! [`set_input_undoable`]/[`set_input_string_undoable`] build an unexecuted
//! `quercus_undo::undocommand::UndoCommand` with the same closure pattern as
//! `quercus_node::ops::set_value_at_time_command`
//! (`command_from_closures`): the redo closure locks project, calls
//! `graph.get_mut(id)` and `NodeCore::set_standard_value` to write the standard
//! value; the undo closure replays the old value snapshotted at creation time.
//! The command is submitted by the caller
//! ([`crate::param::notify_instance_changed`] →
//! [`crate::instance::Instance::submit_undo_command`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use quercus_undo::undocommand::UndoCommand;

/// A quercusnode node reference (single-lib): a value-type replica of the old
/// C ABI handle box `(Arc<Mutex<Project>>, NodeId)`.
#[derive(Clone)]
pub struct NodeRef {
/// Owning project (the node's lifetime follows the project).
	pub project: Arc<Mutex<quercus_node::project::Project>>,
/// The node's id inside project.graph.
	pub id: quercus_node::id::NodeId,
}

/// Values of quercusnode_value_type (node.h:74).
pub mod node_value_type {
/// QUERCUSNODE_VALUE_NONE.
	pub const NONE: i32 = 0;
/// QUERCUSNODE_VALUE_INT.
	pub const INT: i32 = 1;
/// QUERCUSNODE_VALUE_FLOAT.
	pub const FLOAT: i32 = 2;
/// QUERCUSNODE_VALUE_BOOL.
	pub const BOOL: i32 = 3;
/// QUERCUSNODE_VALUE_RATIONAL.
	pub const RATIONAL: i32 = 4;
/// QUERCUSNODE_VALUE_COLOR.
	pub const COLOR: i32 = 5;
/// QUERCUSNODE_VALUE_VEC2.
	pub const VEC2: i32 = 6;
/// QUERCUSNODE_VALUE_VEC3.
	pub const VEC3: i32 = 7;
/// QUERCUSNODE_VALUE_VEC4.
	pub const VEC4: i32 = 8;
/// QUERCUSNODE_VALUE_COMBO.
	pub const COMBO: i32 = 9;
/// QUERCUSNODE_VALUE_STRING.
	pub const STRING: i32 = 10;
}

/// quercusnode_value (include/node/node.h:93, field-for-field identical).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Value {
/// Type ([`node_value_type`]).
	pub r#type: i32,
/// INT/COMBO value, BOOL 0/1, RATIONAL numerator.
	pub num: i64,
/// RATIONAL denominator.
	pub den: i64,
/// FLOAT f[0]; VEC2/3/4 f[0..n-1]; COLOR r,g,b,a.
	pub f: [f64; 4],
}

impl Value {
	/// Construct an INT value POD: `num = v`, `den = 0`, `f` all zeros.
	///
	/// `v` is an `i64` integer with no range check (the consumer interprets it
	/// according to the declared input type). Converted by
	/// [`Value::to_node_value`] into [`quercus_node::value::NodeValue::Int`], the
	/// inverse of the `NV::Int` branch of [`Value::from_node_value`].
	pub const fn int(v: i64) -> Self {
		Self {
			r#type: node_value_type::INT,
			num: v,
			den: 0,
			f: [0.0; 4],
		}
	}

	/// Construct a FLOAT value POD: `f[0] = v`; the remaining components and
	/// `num`/`den` are 0.
	///
	/// `v` is an `f64` scalar with no range or precision check. Converted by
	/// [`Value::to_node_value`] into
	/// [`quercus_node::value::NodeValue::Float`] (only `f[0]` is read).
	pub const fn float(v: f64) -> Self {
		Self {
			r#type: node_value_type::FLOAT,
			num: 0,
			den: 0,
			f: [v, 0.0, 0.0, 0.0],
		}
	}

	/// Construct a BOOL value POD: `num = v as i64` (`false` → 0, `true` → 1),
	/// `den` is 0, `f` is all zeros.
	///
	/// Converted by [`Value::to_node_value`] into
	/// [`quercus_node::value::NodeValue::Boolean`] (the conversion tests
	/// `num != 0`).
	pub const fn bool_(v: bool) -> Self {
		Self {
			r#type: node_value_type::BOOL,
			num: v as i64,
			den: 0,
			f: [0.0; 4],
		}
	}

	/// Construct a COMBO value POD: `num = v`, `den = 0`, `f` all zeros.
	///
	/// `v` is a 0-based choice index, i.e. the position in the input's
	/// `combo_strings`; there is no range check (an out-of-range index is treated as
	/// invalid by the consumer). Converted by [`Value::to_node_value`] into
	/// [`quercus_node::value::NodeValue::Combo`].
	pub const fn combo(v: i64) -> Self {
		Self {
			r#type: node_value_type::COMBO,
			num: v,
			den: 0,
			f: [0.0; 4],
		}
	}

	/// Construct a COLOR value POD: `f = [r, g, b, a]`, `num`/`den` are 0.
	///
	/// The four components are written as-is, with no clamp, gamma or colour space
	/// conversion (the POD is only a carrier; the consumer decides how to interpret
	/// it). Converted by [`Value::to_node_value`] into
	/// [`quercus_node::value::NodeValue::Color`], the inverse of the `NV::Color`
	/// branch of [`Value::from_node_value`].
	pub const fn color(r: f64, g: f64, b: f64, a: f64) -> Self {
		Self {
			r#type: node_value_type::COLOR,
			num: 0,
			den: 0,
			f: [r, g, b, a],
		}
	}

	/// Construct a VEC2/VEC3/VEC4 value POD.
	///
	/// The length of `v` selects the type: 2 → VEC2, 3 → VEC3, anything else
	/// (0/1/4/more than 4) → VEC4. At most the first 4 elements are copied, the
	/// remaining `f` components are 0, and elements beyond 4 are silently dropped;
	/// there is no range check. Converted by [`Value::to_node_value`] into
	/// [`quercus_node::value::NodeValue::Vec2`]/`Vec3`/`Vec4`.
	pub const fn vec(v: &[f64]) -> Self {
		let t = match v.len() {
			2 => node_value_type::VEC2,
			3 => node_value_type::VEC3,
			_ => node_value_type::VEC4,
		};
		let mut f = [0.0; 4];
		let mut i = 0;
		while i < v.len() && i < 4 {
			f[i] = v[i];
			i += 1;
		}
		Self {
			r#type: t,
			num: 0,
			den: 0,
			f,
		}
	}

	/// Typed constructor for the string family (the POD carries no data; the value
	/// travels through [`set_input_string_undoable`]).
	pub const fn string() -> Self {
		Self {
			r#type: node_value_type::STRING,
			num: 0,
			den: 0,
			f: [0.0; 4],
		}
	}

	/// POD → [`quercus_node::value::NodeValue`], mapped by POD kind (matching the
	/// kind branches of `quercus_node::value::QuercusNodeValue::to_node_value`, not
	/// requantised by the declared type). A `STRING`-family POD carries no data →
	/// `None`; `NONE` → [`quercus_node::value::NodeValue::None`].
	pub fn to_node_value(&self) -> Option<quercus_node::value::NodeValue> {
		use quercus_node::value::NodeValue as NV;
		match self.r#type {
			node_value_type::NONE => Some(NV::None),
			node_value_type::INT => Some(NV::Int(self.num)),
			node_value_type::FLOAT => Some(NV::Float(self.f[0])),
			node_value_type::BOOL => Some(NV::Boolean(self.num != 0)),
			node_value_type::RATIONAL => Some(NV::Rational(quercus_core::Rational::new(
				self.num, self.den,
			))),
			node_value_type::COLOR => Some(NV::Color(self.f)),
			node_value_type::VEC2 => Some(NV::Vec2([self.f[0], self.f[1]])),
			node_value_type::VEC3 => Some(NV::Vec3([self.f[0], self.f[1], self.f[2]])),
			node_value_type::VEC4 => Some(NV::Vec4(self.f)),
			node_value_type::COMBO => Some(NV::Combo(self.num)),
			_ => None,
		}
	}

/// [`quercus_node::value::NodeValue`] → POD (the inverse of
/// `ValueType::to_quercus`). Types with no POD representation (string
/// family/texture/sample/matrix, ...) → `None`.
	pub fn from_node_value(v: &quercus_node::value::NodeValue) -> Option<Value> {
		use quercus_node::value::NodeValue as NV;
		Some(match v {
			NV::None => Value {
				r#type: node_value_type::NONE,
				num: 0,
				den: 0,
				f: [0.0; 4],
			},
			NV::Int(i) => Value::int(*i),
			NV::Float(f) => Value::float(*f),
			NV::Boolean(b) => Value::bool_(*b),
			NV::Rational(r) => Value {
				r#type: node_value_type::RATIONAL,
				num: r.numerator(),
				den: r.denominator(),
				f: [0.0; 4],
			},
			NV::Color(c) => Value::color(c[0], c[1], c[2], c[3]),
			NV::Vec2(a) => Value::vec(&[a[0], a[1]]),
			NV::Vec3(a) => Value::vec(&[a[0], a[1], a[2]]),
			NV::Vec4(a) => Value::vec(&[a[0], a[1], a[2], a[3]]),
			NV::Combo(i) => Value::combo(*i),
			_ => return None,
		})
	}
}

// ---- 身份注册表 -----------------------------------------------------------

/// Registry entry: a weak project reference (once the project is released the
/// upgrade fails and the entry goes dead) + the node id.
struct RegistryEntry {
	/// Owning project (weak reference; strong references are held by callers/nodes).
	project: Weak<Mutex<quercus_node::project::Project>>,
	/// The node id.
	id: quercus_node::id::NodeId,
}

/// Process-wide identity registry (lazily initialised OnceLock; registration may repeat within a test process).
static NODE_REGISTRY: OnceLock<Mutex<HashMap<u64, RegistryEntry>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<u64, RegistryEntry>> {
	NODE_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a quercusnode node (called during facade assembly; the registration
/// side of the M9 C++ `quercusnode_node_identity()` registry). The returned
/// registry key is the packed identity + 1
/// ([`quercus_node::id::NodeId::identity`]): identity 0 is a valid first node
/// while `Instance::node_identity` uses 0 as the unbound sentinel, so the
/// shifted values never collide. The key is written into
/// [`crate::instance::Instance::bind_node`], and removal and lookup
/// ([`node_from_identity`]) use the same key. Registering the same identity
/// again overwrites the old entry (re-binding).
pub fn register_node(
	project: Arc<Mutex<quercus_node::project::Project>>,
	id: quercus_node::id::NodeId,
) -> u64 {
	let identity = id
		.identity()
		.checked_add(1)
		.expect("register_node: the invalid node id has no registry key");
	let entry = RegistryEntry {
		project: Arc::downgrade(&project),
		id,
	};
	registry()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(identity, entry);
	identity
}

/// Remove the entry for `identity` from the process-wide identity registry.
///
/// Called from the node destruction path; an unregistered identity is ignored
/// (a no-op, not an error). Uses the same key as [`register_node`]
/// (`NodeId::identity() + 1`). Only the weak-reference entry is removed; the
/// project and the node itself are untouched, and a [`NodeRef`] already obtained
/// stays usable.
pub fn unregister_node(identity: u64) {
	registry()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.remove(&identity);
}

/// Identity → node reference (the param write-back entry point).
///
/// `None`: the identity is not registered, or the project has been released
/// (the weak upgrade fails — the explainable failure path the old C ABI had for
/// dangling handles). The caller ([`crate::param::notify_instance_changed`])
/// treats it as "unbound node → no-op".
pub fn node_from_identity(identity: usize) -> Option<NodeRef> {
	let (project, id) = {
		let reg = registry()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let entry = reg.get(&(identity as u64))?;
		(entry.project.clone(), entry.id)
	};
	let project = project.upgrade()?;
	Some(NodeRef { project, id })
}

// ---- 桥调用面（undoable 回写）---------------------------------------------

/// quercusnode bridge error (why a set_input* failed; the caller skips submission on failure).
#[derive(Debug, thiserror::Error)]
pub enum NodeBridgeError {
	/// The node id is stale (no such node in the graph).
	#[error("node id is stale or not present in the graph")]
	StaleNode,
	/// The node has no such input.
	#[error("node has no input '{0}'")]
	UnknownInput(String),
	/// The value POD cannot be converted (STRING-family PODs carry no data; the numeric family always converts).
	#[error("value POD of type {0} cannot be converted to a node value")]
	InvalidValue(i32),
	/// The input is not a string-family type (the string setter is limited to Text/StrCombo).
	#[error("input '{0}' is not a string-family input")]
	NotStringInput(String),
}

/// Take the mutex; on poisoning, adopt the inner value and keep using it (`PoisonError::into_inner`).
///
/// Shared by every short critical section in this module: one panic will not
/// cascade into later call failures, at the cost of possibly observing state
/// written just before the panic. The only difference from
/// [`std::sync::Mutex::lock`] is that it never returns `Err`, so callers need no
/// poisoned-lock branch.
fn lock_any<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Build an unexecuted [`UndoCommand`] from redo/undo closures (closure pattern,
/// isomorphic to `quercus_node::ops::command_from_closures`).
fn command_from_closures(
	redo: impl FnMut() + Send + 'static,
	undo: impl FnMut() + Send + 'static,
) -> UndoCommand {
	UndoCommand::from_closures(redo, undo)
}

/// Set a node's standard input value undoably (POD path; numeric-family inputs).
///
/// Returns an **unexecuted** [`UndoCommand`] (the caller
/// [`crate::instance::Instance::submit_undo_command`] submits it): redo locks
/// project, calls `graph.get_mut(id)` and `NodeCore::set_standard_value` to write
/// the new value; undo replays the old value snapshotted at creation time
/// (isomorphic to the standard-value branch of
/// `quercus_node::ops::set_value_at_time_command`, element = -1 for the whole
/// value). When the node or input is stale it returns [`NodeBridgeError`] and
/// produces no command.
pub fn set_input_undoable(
	node: &NodeRef,
	input: &str,
	value: &Value,
) -> Result<UndoCommand, NodeBridgeError> {
	let nv = value
		.to_node_value()
		.ok_or(NodeBridgeError::InvalidValue(value.r#type))?;
	// 校验节点与输入存在性 + 旧值快照（创建期状态，redo/undo 据此
	// 回放；不校验声明类型——按 POD kind 存原样，与 ops.rs 一致）。
	let old = {
		let mut guard = lock_any(&node.project);
		let entry = guard
			.graph
			.get_mut(node.id)
			.ok_or(NodeBridgeError::StaleNode)?;
		if entry.core.input_data_type(input).is_none() {
			return Err(NodeBridgeError::UnknownInput(input.to_string()));
		}
		entry.core.standard_value(input, -1)
	};

	let project = node.project.clone();
	let id = node.id;
	let input_redo = input.to_string();
	let input_undo = input.to_string();
	let value_redo = nv.clone();
	let project_redo = project.clone();
	let project_undo = project.clone();
	Ok(command_from_closures(
		move || {
			let mut guard = lock_any(&project_redo);
			if let Some(entry) = guard.graph.get_mut(id) {
				entry
					.core
					.set_standard_value(&input_redo, -1, value_redo.clone());
			}
		},
		move || {
			let mut guard = lock_any(&project_undo);
			if let Some(entry) = guard.graph.get_mut(id) {
				entry
					.core
					.set_standard_value(&input_undo, -1, old.clone());
			}
		},
	))
}

/// Set a node's standard input value undoably (string-family inputs: Text /
/// StrCombo / Parametric — the POD carries no string data).
///
/// The storage variant follows the input's declared type
/// ([`quercus_node::value::ValueType`]): `StrCombo` →
/// [`quercus_node::value::NodeValue::StrCombo`], `Text` / `Parametric` (the value
/// is curve JSON text) → [`quercus_node::value::NodeValue::Text`]. A declared
/// type outside the string family → [`NodeBridgeError::NotStringInput`]. Command
/// semantics are the same as [`set_input_undoable`].
pub fn set_input_string_undoable(
	node: &NodeRef,
	input: &str,
	value: &str,
) -> Result<UndoCommand, NodeBridgeError> {
	let (old, declared) = {
		let guard = lock_any(&node.project);
		let entry = guard
			.graph
			.get(node.id)
			.ok_or(NodeBridgeError::StaleNode)?;
		let declared = entry
			.core
			.input_data_type(input)
			.ok_or_else(|| NodeBridgeError::UnknownInput(input.to_string()))?;
		(entry.core.standard_value(input, -1), declared)
	};
	let nv = match declared {
		quercus_node::value::ValueType::StrCombo => quercus_node::value::NodeValue::StrCombo(value.to_string()),
		quercus_node::value::ValueType::Text | quercus_node::value::ValueType::Parametric => {
			quercus_node::value::NodeValue::Text(value.to_string())
		}
		_ => return Err(NodeBridgeError::NotStringInput(input.to_string())),
	};

	let project = node.project.clone();
	let id = node.id;
	let input_redo = input.to_string();
	let input_undo = input.to_string();
	let value_redo = nv.clone();
	let project_redo = project.clone();
	let project_undo = project.clone();
	Ok(command_from_closures(
		move || {
			let mut guard = lock_any(&project_redo);
			if let Some(entry) = guard.graph.get_mut(id) {
				entry
					.core
					.set_standard_value(&input_redo, -1, value_redo.clone());
			}
		},
		move || {
			let mut guard = lock_any(&project_undo);
			if let Some(entry) = guard.graph.get_mut(id) {
				entry
					.core
					.set_standard_value(&input_undo, -1, old.clone());
			}
		},
	))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Serializes the identity-registry tests: every `test_project()` node
	/// gets the same packed `NodeId::identity` (a fresh graph's first
	/// node), so two tests running in parallel collide in the process-wide
	/// registry and one resolves the other's live entry.
	fn identity_lock() -> &'static std::sync::Mutex<()> {
		static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
		&LOCK
	}

	/// Build a test node with Text/StrCombo/Float inputs.
	fn test_project() -> (Arc<Mutex<quercus_node::project::Project>>, quercus_node::id::NodeId) {
		let project = quercus_node::project::Project::new();
		let mut guard = project.lock().unwrap();
		let id = guard.graph.add_node(
			quercus_node::node::NodeCore::empty(),
			Box::new(quercus_node::nodes::EmptyBehavior),
		);
		// NodeCore::empty 无输入；手动声明三种输入。
		use quercus_node::input::Input;
		let core = guard.graph.get_mut(id).unwrap();
		core.core.add_input(Input::new(
			"label",
			quercus_node::value::ValueType::Text,
			quercus_node::value::NodeValue::Text(String::new()),
		));
		core.core.add_input(Input::new(
			"choice",
			quercus_node::value::ValueType::StrCombo,
			quercus_node::value::NodeValue::StrCombo(String::new()),
		));
		core.core.add_input(Input::new(
			"opacity",
			quercus_node::value::ValueType::Float,
			quercus_node::value::NodeValue::Float(1.0),
		));
		drop(guard);
		(project, id)
	}

	/// Registry: the full register → node_from_identity → unregister chain.
	#[test]
	fn identity_register_roundtrip() {
		let _guard = identity_lock().lock().unwrap_or_else(|e| e.into_inner());
		let (project, id) = test_project();
		let identity = register_node(project.clone(), id);
		let node = node_from_identity(identity as usize).expect("已登记身份应可解析");
		assert_eq!(node.id, id);
		let found = node.project.lock().unwrap().graph.get(id).is_some();
		assert!(found);

		// 未登记身份 → None。
		assert!(node_from_identity(0xfeed_face).is_none());
		unregister_node(identity);
		assert!(node_from_identity(identity as usize).is_none());
	}

	/// Once the project is released the weak entry fails to upgrade → None
	/// (dangling semantics); re-registering the same slot overwrites the dead entry
	/// and resolves to the new project.
	#[test]
	fn identity_project_dropped() {
		let _guard = identity_lock().lock().unwrap_or_else(|e| e.into_inner());
		let (project, id) = test_project();
		let identity = register_node(project.clone(), id);
		drop(project);
		assert!(node_from_identity(identity as usize).is_none());
		// 新 project 同槽位身份重登记（覆盖死条目）。
		let (project2, id2) = test_project();
		register_node(project2.clone(), id2);
		let node = node_from_identity(identity as usize).expect("重登记后应可解析");
		assert_eq!(node.id, id2);
		let alive = node.project.lock().unwrap().graph.get(id2).is_some();
		assert!(alive);
	}

	/// Full Value POD ↔ NodeValue mapping; STRING-family PODs carry no data.
	#[test]
	fn pod_node_value_roundtrip() {
		use quercus_node::value::NodeValue as NV;
		let cases: Vec<(Value, NV)> = vec![
			(Value::int(7), NV::Int(7)),
			(Value::float(1.5), NV::Float(1.5)),
			(Value::bool_(true), NV::Boolean(true)),
			(Value::combo(3), NV::Combo(3)),
			(
				Value {
					r#type: node_value_type::RATIONAL,
					num: 3,
					den: 2,
					f: [0.0; 4],
				},
				NV::Rational(quercus_core::Rational::new(3, 2)),
			),
			(Value::color(0.1, 0.2, 0.3, 0.4), NV::Color([0.1, 0.2, 0.3, 0.4])),
			(Value::vec(&[1.0, 2.0]), NV::Vec2([1.0, 2.0])),
			(Value::vec(&[1.0, 2.0, 3.0]), NV::Vec3([1.0, 2.0, 3.0])),
			(
				Value::vec(&[1.0, 2.0, 3.0, 4.0]),
				NV::Vec4([1.0, 2.0, 3.0, 4.0]),
			),
		];
		for (pod, nv) in cases {
			let to = pod.to_node_value().expect("POD 应可转换");
			assert_eq!(to, nv, "POD → NodeValue");
			assert_eq!(Value::from_node_value(&nv), Some(pod), "NodeValue → POD");
		}
		// STRING POD 无数据。
		assert!(Value::string().to_node_value().is_none());
		// 无 POD 表达的类型。
		assert_eq!(Value::from_node_value(&NV::Text("x".into())), None);
		assert_eq!(Value::from_node_value(&NV::Matrix([0.0; 16])), None);
	}

	/// set_input_undoable: redo writes the standard value, undo replays the old value; error paths.
	#[test]
	fn set_input_undoable_applies_and_undoes() {
		let (project, id) = test_project();
		let node = NodeRef {
			project: project.clone(),
			id,
		};
		let mut cmd = set_input_undoable(&node, "opacity", &Value::float(0.25)).unwrap();
		// 命令未执行时值未变。
		assert_eq!(
			project
				.lock()
				.unwrap()
				.graph
				.get(id)
				.unwrap()
				.core
				.standard_value("opacity", -1),
			quercus_node::value::NodeValue::Float(1.0)
		);
		cmd.redo_now();
		assert_eq!(
			project
				.lock()
				.unwrap()
				.graph
				.get(id)
				.unwrap()
				.core
				.standard_value("opacity", -1),
			quercus_node::value::NodeValue::Float(0.25)
		);
		cmd.undo_now();
		assert_eq!(
			project
				.lock()
				.unwrap()
				.graph
				.get(id)
				.unwrap()
				.core
				.standard_value("opacity", -1),
			quercus_node::value::NodeValue::Float(1.0)
		);

		// 未知输入 / STRING POD / 失效节点。
		assert!(matches!(
			set_input_undoable(&node, "nope", &Value::float(1.0)),
			Err(NodeBridgeError::UnknownInput(_))
		));
		assert!(matches!(
			set_input_undoable(&node, "opacity", &Value::string()),
			Err(NodeBridgeError::InvalidValue(_))
		));
		let stale = NodeRef {
			project,
			id: quercus_node::id::NodeId::from_identity(999).unwrap(),
		};
		assert!(matches!(
			set_input_undoable(&stale, "opacity", &Value::float(1.0)),
			Err(NodeBridgeError::StaleNode)
		));
	}

	/// set_input_string_undoable: Text/StrCombo variant selection and replay.
	#[test]
	fn set_input_string_undoable_variants() {
		let (project, id) = test_project();
		let node = NodeRef {
			project: project.clone(),
			id,
		};
		let mut cmd = set_input_string_undoable(&node, "label", "hello").unwrap();
		cmd.redo_now();
		assert_eq!(
			project
				.lock()
				.unwrap()
				.graph
				.get(id)
				.unwrap()
				.core
				.standard_value("label", -1),
			quercus_node::value::NodeValue::Text("hello".to_string())
		);
		cmd.undo_now();
		assert_eq!(
			project
				.lock()
				.unwrap()
				.graph
				.get(id)
				.unwrap()
				.core
				.standard_value("label", -1),
			quercus_node::value::NodeValue::Text(String::new())
		);

		let mut cmd = set_input_string_undoable(&node, "choice", "low").unwrap();
		cmd.redo_now();
		assert_eq!(
			project
				.lock()
				.unwrap()
				.graph
				.get(id)
				.unwrap()
				.core
				.standard_value("choice", -1),
			quercus_node::value::NodeValue::StrCombo("low".to_string())
		);

		// 数值输入走字符串 setter → NotStringInput。
		assert!(matches!(
			set_input_string_undoable(&node, "opacity", "1.0"),
			Err(NodeBridgeError::NotStringInput(_))
		));
	}
}
