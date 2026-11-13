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

//! Contract tests for property.rs: OFX property-set semantics.
//!
//! Reference: HS: ofxhProperty.cpp. Semantic points: multi-dimensional
//! arrays, behavior for out-of-bounds/missing/type-mismatched access, and
/// define replace semantics.
mod common;

use std::ffi::{c_void, CString};
use std::sync::Arc;

use quercus_ofx_plugin::error::Error;
use quercus_ofx_plugin::property::{PropertySet, Value};

/// `Value` does not implement PartialEq (Pointer cannot be compared), so the
/// tests assert value equality by comparing Debug strings (the Debug output
/// of in-process pointers is deterministic).
fn val_eq(a: &Value, b: &Value) -> bool {
	format!("{a:?}") == format!("{b:?}")
}

/// After define, get hits; defining the same name replaces the whole value
/// array (the dimension changes with it).
#[test]
fn define_and_replace() {
	let s = PropertySet::new();
	s.define("a", vec![Value::Int(1), Value::Int(2)]);
	assert_eq!(s.dimension("a"), 2);
	assert!(val_eq(&s.get("a", 0).unwrap(), &Value::Int(1)));

	// 同名整体替换：旧值数组连同维度一起被覆盖。
	s.define("a", vec![Value::Int(9)]);
	assert_eq!(s.dimension("a"), 1);
	assert!(val_eq(&s.get("a", 0).unwrap(), &Value::Int(9)));
	assert!(s.get("a", 1).is_none());
}

/// get on an undefined property returns None; dimension is 0.
#[test]
fn missing_property() {
	let s = PropertySet::new();
	assert!(s.get("nope", 0).is_none());
	assert!(s.get("nope", 42).is_none());
	assert_eq!(s.dimension("nope"), 0);
}

/// Out-of-bounds read (index >= dimension) returns None.
#[test]
fn out_of_bounds_read() {
	let s = PropertySet::new();
	s.define("a", vec![Value::Int(1)]);
	assert!(s.get("a", 1).is_none());
	assert!(s.get("a", 100).is_none());
}

/// set_at overwrites the given index without changing the dimension; on a
/// missing property it returns a NotFound error.
#[test]
fn set_at_semantics() {
	let s = PropertySet::new();
	s.define("a", vec![Value::Int(1), Value::Int(2)]);

	// 覆盖命中下标，维度不变。
	assert!(s.set_at("a", 0, Value::Int(9)).is_ok());
	assert!(val_eq(&s.get("a", 0).unwrap(), &Value::Int(9)));
	assert_eq!(s.dimension("a"), 2);

	// 越界写：维度固定语义（不自动扩容）→ NotFound。
	assert!(matches!(
		s.set_at("a", 2, Value::Int(3)),
		Err(Error::NotFound)
	));
	assert_eq!(s.dimension("a"), 2);

	// 缺失属性：NotFound。
	assert!(matches!(
		s.set_at("b", 0, Value::Int(1)),
		Err(Error::NotFound)
	));
}

/// Round-trip for each of the four value types (Int/Double/String/Pointer);
/// strings include the empty string and non-ASCII UTF-8.
#[test]
fn typed_roundtrip() {
	let s = PropertySet::new();
	let non_ascii = CString::new("你好，wörld ✓").unwrap();
	let raw = 0x1234usize as *mut c_void;

	s.define("ints", vec![Value::Int(-7)]);
	s.define("dbls", vec![Value::Double(1.5)]);
	s.define("strs", vec![Value::String(non_ascii.clone())]);
	s.define("ptrs", vec![Value::Pointer(raw)]);

	assert!(val_eq(&s.get("ints", 0).unwrap(), &Value::Int(-7)));
	assert!(val_eq(&s.get("dbls", 0).unwrap(), &Value::Double(1.5)));
	assert!(val_eq(
		&s.get("strs", 0).unwrap(),
		&Value::String(non_ascii)
	));
	assert!(val_eq(&s.get("ptrs", 0).unwrap(), &Value::Pointer(raw)));

	// 空串也是合法属性值。
	s.set_one("empty", Value::String(CString::new("").unwrap()));
	assert!(val_eq(
		&s.get("empty", 0).unwrap(),
		&Value::String(CString::new("").unwrap())
	));
}

/// Properties disappear after remove; removing a missing property is a
/// no-op.
#[test]
fn remove_semantics() {
	let s = PropertySet::new();
	s.set_one("a", Value::Int(1));
	s.remove("a");
	assert!(s.get("a", 0).is_none());
	assert_eq!(s.dimension("a"), 0);

	// 缺失属性 remove：no-op，不崩。
	s.remove("a");
	s.remove("never-existed");
}

/// snapshot returns a full snapshot isolated from later modifications (deep
/// copy).
#[test]
fn snapshot_is_isolated() {
	let s = PropertySet::new();
	s.define("a", vec![Value::Int(1)]);

	let snap = s.snapshot();
	assert_eq!(snap.len(), 1);
	assert_eq!(snap[0].name, "a");

	// 后续对属性集的修改不影响快照。
	s.set_at("a", 0, Value::Int(2)).unwrap();
	s.set_one("b", Value::Int(3));
	assert_eq!(snap.len(), 1);
	assert!(val_eq(&snap[0].values[0], &Value::Int(1)));

	// 反向隔离：改快照不回写属性集（深拷贝验证）。
	let mut snap = snap;
	snap[0].values[0] = Value::Int(99);
	assert!(val_eq(&s.get("a", 0).unwrap(), &Value::Int(2)));
}

/// Concurrent reads and writes (32 threads interleaving define/get/set_at) do
/// not crash and the data stays consistent (the property set is shared by all
/// suites and must be thread-safe).
#[test]
fn concurrent_access() {
	// 线程要求 'static：经 Arc 共享属性集（对应插件线程经句柄共享）。
	let s = Arc::new(PropertySet::new());
	let names: Vec<&'static str> = (0..32).map(leak_name).collect();
	for (i, name) in names.iter().enumerate() {
		s.set_one(name, Value::Int(i as i32));
	}

	let threads: Vec<_> = (0..32usize)
		.map(|i| {
			let s = Arc::clone(&s);
			// &'static str 是 Copy：拷贝进闭包，线程各自持名。
			let name = names[i];
			std::thread::spawn(move || {
				for k in 0..500 {
					// 读永远合法（属性始终存在、维度恒为 1）。
					assert!(s.get(name, 0).is_some());
					if i % 2 == 0 {
						// 偶数线程：set_at 覆盖（不改维度）。
						s.set_at(name, 0, Value::Int((i + k) as i32)).unwrap();
					} else {
						// 奇数线程：define 整体替换（维度同样恒为 1）。
						s.define(name, vec![Value::Int((i as i32) - k as i32)]);
					}
				}
			})
		})
		.collect();
	for t in threads {
		t.join().unwrap();
	}

	// 数据自洽：32 个属性全部存活、维度未被撑大。
	for name in names.iter() {
		assert_eq!(s.dimension(name), 1);
		assert!(s.get(name, 0).is_some());
	}
}

/// Leak a `&'static str` property name (for the concurrency test; reclaimed
/// when the test process exits).
fn leak_name(i: usize) -> &'static str {
	Box::leak(format!("p{i}").into_boxed_str())
}
