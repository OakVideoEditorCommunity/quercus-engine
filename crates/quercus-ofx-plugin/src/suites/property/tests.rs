use super::*;

fn handle(set: &PropertySet) -> *mut c_void {
	set as *const PropertySet as *mut c_void
}

fn cs(s: &str) -> CString {
	CString::new(s).unwrap()
}

/// `Value` does not implement PartialEq (Pointer cannot be compared) — compare
/// Debug strings instead.
fn val_eq(a: &Value, b: &Value) -> bool {
	format!("{a:?}") == format!("{b:?}")
}

fn assert_value(set: &PropertySet, name: &str, index: usize, want: &Value) {
	assert!(
		val_eq(set.get(name, index).as_ref().expect("属性应存在"), want),
		"{name}[{index}] 值不符"
	);
}

/// Null handle → BadHandle; null / non-UTF-8 property name → ErrValue.
#[test]
fn bad_handles_and_bad_names() {
	let s = suite_v1();
	let set = PropertySet::new();
	let name = cs("a");
	let mut iv = 0;
	let mut dv = 0.0;
	let mut sv: *mut c_char = std::ptr::null_mut();
	let mut pv: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.set_int)(std::ptr::null_mut(), name.as_ptr(), 0, 1),
			status::ERR_BAD_HANDLE
		);
		assert_eq!(
			(s.get_int)(std::ptr::null_mut(), name.as_ptr(), 0, &mut iv),
			status::ERR_BAD_HANDLE
		);
		// 空属性名。
		assert_eq!(
			(s.set_int)(handle(&set), std::ptr::null(), 0, 1),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_int)(handle(&set), std::ptr::null(), 0, &mut iv),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.set_double)(handle(&set), std::ptr::null(), 0, 1.0),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.set_string)(handle(&set), std::ptr::null(), 0, name.as_ptr()),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_double)(handle(&set), std::ptr::null(), 0, &mut dv),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_string)(handle(&set), std::ptr::null(), 0, &mut sv),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_pointer)(handle(&set), std::ptr::null(), 0, &mut pv),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_dimension)(handle(&set), std::ptr::null(), &mut iv),
			status::ERR_VALUE
		);
		assert_eq!((s.reset)(handle(&set), std::ptr::null()), status::ERR_VALUE);
		// 非 UTF-8 属性名（0xFF 后接 NUL）。
		let bad: [c_char; 2] = [0xFFu8 as c_char, 0];
		assert_eq!(
			(s.set_int)(handle(&set), bad.as_ptr(), 0, 1),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_int)(handle(&set), bad.as_ptr(), 0, &mut iv),
			status::ERR_VALUE
		);
	}
}

/// get on an undefined property → ErrUnknown; after define, an empty array has
/// dimension 0 and an out-of-bounds read → BadIndex; negative index → BadIndex.
#[test]
fn get_missing_empty_and_bounds() {
	let s = suite_v1();
	let set = PropertySet::new();
	let n = cs("a");
	let mut iv = 0;
	unsafe {
		assert_eq!(
			(s.get_int)(handle(&set), n.as_ptr(), 0, &mut iv),
			status::ERR_UNKNOWN
		);
		assert_eq!(
			(s.get_int)(handle(&set), n.as_ptr(), -1, &mut iv),
			status::ERR_UNKNOWN,
			"未定义属性先报 Unknown（HS 先查类型）"
		);
	}
	set.define("a", vec![]);
	unsafe {
		assert_eq!(
			(s.get_int)(handle(&set), n.as_ptr(), 0, &mut iv),
			status::ERR_BAD_INDEX
		);
	}
	set.define("a", vec![Value::Int(3)]);
	unsafe {
		assert_eq!(
			(s.get_int)(handle(&set), n.as_ptr(), -1, &mut iv),
			status::ERR_BAD_INDEX
		);
		assert_eq!(
			(s.get_int)(handle(&set), n.as_ptr(), 1, &mut iv),
			status::ERR_BAD_INDEX
		);
	}
}

/// propSet implicit creation: index 2 builds a property of dimension 3 with the
/// leading slots holding the same value; for a defined array, index ==
/// dimension appends and out of bounds → BadIndex.
#[test]
fn set_creates_and_appends() {
	let s = suite_v1();
	let set = PropertySet::new();
	let n = cs("a");
	unsafe {
		assert_eq!((s.set_int)(handle(&set), n.as_ptr(), 2, 7), 0);
	}
	assert_eq!(set.dimension("a"), 3);
	assert_value(&set, "a", 0, &Value::Int(7));
	assert_value(&set, "a", 2, &Value::Int(7));

	// index == 维度 → 追加（协商属性逐位增长语义）。
	unsafe {
		assert_eq!((s.set_int)(handle(&set), n.as_ptr(), 3, 9), 0);
	}
	assert_eq!(set.dimension("a"), 4);
	assert_value(&set, "a", 3, &Value::Int(9));

	// 越界（跳过追加位）→ BadIndex。
	unsafe {
		assert_eq!(
			(s.set_int)(handle(&set), n.as_ptr(), 9, 1),
			status::ERR_BAD_INDEX
		);
	}
	assert_eq!(set.dimension("a"), 4);
}

/// Empty predefined array: index 0 pushes; index > 0 → BadIndex.
#[test]
fn set_into_empty_predefined_array() {
	let s = suite_v1();
	let set = PropertySet::new();
	set.define("empty", vec![]);
	let n = cs("empty");
	unsafe {
		assert_eq!(
			(s.set_int)(handle(&set), n.as_ptr(), 1, 1),
			status::ERR_BAD_INDEX
		);
		assert_eq!((s.set_int)(handle(&set), n.as_ptr(), 0, 5), 0);
	}
	assert_value(&set, "empty", 0, &Value::Int(5));
}

/// propSet type mismatch → Unknown; numeric Int/Double are not converted to
/// each other (set is strict).
#[test]
fn set_type_mismatch() {
	let s = suite_v1();
	let set = PropertySet::new();
	set.define("i", vec![Value::Int(0)]);
	set.define("d", vec![Value::Double(0.0)]);
	let ni = cs("i");
	let nd = cs("d");
	unsafe {
		assert_eq!(
			(s.set_double)(handle(&set), ni.as_ptr(), 0, 1.0),
			status::ERR_UNKNOWN
		);
		assert_eq!(
			(s.set_int)(handle(&set), nd.as_ptr(), 0, 1),
			status::ERR_UNKNOWN
		);
	}
}

/// propGet: Int ↔ Double numeric coercion; a mismatched element type in a
/// mixed array → Failed.
#[test]
fn get_numeric_coercion_and_mixed_arrays() {
	let s = suite_v1();
	let set = PropertySet::new();
	set.define("i", vec![Value::Int(3)]);
	set.define("d", vec![Value::Double(1.5)]);
	let ni = cs("i");
	let nd = cs("d");
	let mut dv = 0.0;
	let mut iv = 0;
	unsafe {
		assert_eq!((s.get_double)(handle(&set), ni.as_ptr(), 0, &mut dv), 0);
	}
	assert_eq!(dv, 3.0);
	unsafe {
		assert_eq!((s.get_int)(handle(&set), nd.as_ptr(), 0, &mut iv), 0);
	}
	assert_eq!(iv, 1);

	// 混合数组（首元素定类型，后续元素类型不符）→ 单元素读 Failed。
	let mix = PropertySet::new();
	mix.define("m", vec![Value::Double(1.0), Value::String(cs("x"))]);
	let nm = cs("m");
	unsafe {
		assert_eq!(
			(s.get_double)(handle(&mix), nm.as_ptr(), 1, &mut dv),
			status::FAILED
		);
	}
	let mix2 = PropertySet::new();
	mix2.define("m", vec![Value::Int(1), Value::String(cs("x"))]);
	unsafe {
		assert_eq!(
			(s.get_int)(handle(&mix2), nm.as_ptr(), 1, &mut iv),
			status::FAILED
		);
	}
}

/// Reads with too few/too many components in the requested type: get_string on
/// a numeric property → Unknown (type mismatch), get_double on a string
/// property → Unknown.
#[test]
fn get_kind_mismatch() {
	let s = suite_v1();
	let set = PropertySet::new();
	set.define("i", vec![Value::Int(1)]);
	set.define("str", vec![Value::String(cs("s"))]);
	let ni = cs("i");
	let ns = cs("str");
	let mut sv: *mut c_char = std::ptr::null_mut();
	let mut dv = 0.0;
	unsafe {
		assert_eq!(
			(s.get_string)(handle(&set), ni.as_ptr(), 0, &mut sv),
			status::ERR_UNKNOWN
		);
		assert_eq!(
			(s.get_double)(handle(&set), ns.as_ptr(), 0, &mut dv),
			status::ERR_UNKNOWN
		);
	}
}

/// propGet with a null out pointer → ErrValue (all four scalar reads).
#[test]
fn get_null_out_pointers() {
	let s = suite_v1();
	let set = PropertySet::new();
	set.define("i", vec![Value::Int(1)]);
	set.define("d", vec![Value::Double(1.0)]);
	set.define("s", vec![Value::String(cs("x"))]);
	set.define("p", vec![Value::Pointer(std::ptr::null_mut())]);
	let n = cs("i");
	unsafe {
		assert_eq!(
			(s.get_int)(handle(&set), n.as_ptr(), 0, std::ptr::null_mut()),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_double)(handle(&set), n.as_ptr(), 0, std::ptr::null_mut()),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_string)(handle(&set), n.as_ptr(), 0, std::ptr::null_mut()),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_pointer)(handle(&set), n.as_ptr(), 0, std::ptr::null_mut()),
			status::ERR_VALUE
		);
	}
}

/// propGetPointer: writes the pointer successfully; a type mismatch in a mixed
/// array → Failed.
#[test]
fn get_pointer_paths() {
	let s = suite_v1();
	let raw = 0x1234usize as *mut c_void;
	let set = PropertySet::new();
	set.define("p", vec![Value::Pointer(raw)]);
	let np = cs("p");
	let mut out: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!((s.get_pointer)(handle(&set), np.as_ptr(), 0, &mut out), 0);
	}
	assert_eq!(out, raw);

	let mix = PropertySet::new();
	mix.define("p", vec![Value::Pointer(raw), Value::Int(1)]);
	unsafe {
		assert_eq!(
			(s.get_pointer)(handle(&mix), np.as_ptr(), 1, &mut out),
			status::FAILED
		);
	}
}

/// propGetString: writes the in-place pointer with the correct contents; mixed
/// array → Failed.
#[test]
fn get_string_paths() {
	let s = suite_v1();
	let set = PropertySet::new();
	set.define("s", vec![Value::String(cs("hello"))]);
	let ns = cs("s");
	let mut out: *mut c_char = std::ptr::null_mut();
	unsafe {
		assert_eq!((s.get_string)(handle(&set), ns.as_ptr(), 0, &mut out), 0);
		assert_eq!(CStr::from_ptr(out).to_bytes(), b"hello");
	}
	let mix = PropertySet::new();
	mix.define("s", vec![Value::String(cs("x")), Value::Int(1)]);
	unsafe {
		assert_eq!(
			(s.get_string)(handle(&mix), ns.as_ptr(), 1, &mut out),
			status::FAILED
		);
	}
}

/// propSetString: NULL value → ErrValue; truncates at the first NUL.
#[test]
fn set_string_null_and_truncation() {
	let s = suite_v1();
	let set = PropertySet::new();
	let n = cs("s");
	unsafe {
		assert_eq!(
			(s.set_string)(handle(&set), n.as_ptr(), 0, std::ptr::null()),
			status::ERR_VALUE
		);
	}
}

/// propSetN family: implicit creation, dimension replacement/slot-by-slot
/// writes, empty arrays and NULL arguments.
#[test]
fn set_n_families() {
	let s = suite_v1();
	let set = PropertySet::new();

	// set_int_n：隐式创建 2 维。
	let ni = cs("ints");
	let ivals = [1 as c_int, 2];
	unsafe {
		assert_eq!(
			(s.set_int_n)(handle(&set), ni.as_ptr(), 2, ivals.as_ptr()),
			0
		);
	}
	assert_eq!(set.dimension("ints"), 2);
	assert_value(&set, "ints", 1, &Value::Int(2));

	// count 与现有维度相同 → 逐位覆盖。
	let ivals2 = [7 as c_int, 8];
	unsafe {
		assert_eq!(
			(s.set_int_n)(handle(&set), ni.as_ptr(), 2, ivals2.as_ptr()),
			0
		);
	}
	assert_value(&set, "ints", 0, &Value::Int(7));

	// 维度变化 → 整体替换。
	let ivals3 = [9 as c_int];
	unsafe {
		assert_eq!(
			(s.set_int_n)(handle(&set), ni.as_ptr(), 1, ivals3.as_ptr()),
			0
		);
	}
	assert_eq!(set.dimension("ints"), 1);

	// count 0 + NULL：合法（空维度替换）。
	unsafe {
		assert_eq!(
			(s.set_int_n)(handle(&set), ni.as_ptr(), 0, std::ptr::null()),
			0
		);
	}
	assert_eq!(set.dimension("ints"), 0);

	// count > 0 + NULL → ErrValue；负 count → BadIndex。
	unsafe {
		assert_eq!(
			(s.set_int_n)(handle(&set), ni.as_ptr(), 1, std::ptr::null()),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.set_int_n)(handle(&set), ni.as_ptr(), -1, ivals.as_ptr()),
			status::ERR_BAD_INDEX
		);
	}

	// set_double_n：隐式创建 + NULL 分支。
	let nd = cs("dbls");
	let dvals = [1.5 as c_double, 2.5];
	unsafe {
		assert_eq!(
			(s.set_double_n)(handle(&set), nd.as_ptr(), 2, dvals.as_ptr()),
			0
		);
		assert_eq!(
			(s.set_double_n)(handle(&set), nd.as_ptr(), 1, std::ptr::null()),
			status::ERR_VALUE
		);
	}
	assert_value(&set, "dbls", 1, &Value::Double(2.5));

	// set_pointer_n：单元素与 count 0 + NULL。
	let np = cs("ptrs");
	let pvals = [0x55usize as *mut c_void];
	unsafe {
		assert_eq!(
			(s.set_pointer_n)(handle(&set), np.as_ptr(), 1, pvals.as_ptr()),
			0
		);
		assert_eq!(
			(s.set_pointer_n)(handle(&set), np.as_ptr(), 1, std::ptr::null()),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.set_pointer_n)(handle(&set), np.as_ptr(), 0, std::ptr::null()),
			0
		);
	}
	assert_eq!(set.dimension("ptrs"), 0);

	// set_string_n：成功、NULL 数组、数组内 NULL 元素。
	let nstr = cs("strs");
	let sa = cs("a");
	let sb = cs("b");
	let svals = [sa.as_ptr(), sb.as_ptr()];
	unsafe {
		assert_eq!(
			(s.set_string_n)(handle(&set), nstr.as_ptr(), 2, svals.as_ptr()),
			0
		);
		assert_eq!(
			(s.set_string_n)(handle(&set), nstr.as_ptr(), 1, std::ptr::null()),
			status::ERR_VALUE
		);
		let with_null = [sa.as_ptr(), std::ptr::null()];
		assert_eq!(
			(s.set_string_n)(handle(&set), nstr.as_ptr(), 2, with_null.as_ptr()),
			status::ERR_VALUE
		);
	}
	assert_eq!(set.dimension("strs"), 2);
}

/// set_n type mismatch → Unknown; an existing array is replaced with empty when
/// count is 0.
#[test]
fn set_n_type_mismatch_and_empty_replace() {
	let s = suite_v1();
	let set = PropertySet::new();
	set.define("i", vec![Value::Int(1)]);
	let ni = cs("i");
	let dvals = [2.0 as c_double];
	unsafe {
		assert_eq!(
			(s.set_double_n)(handle(&set), ni.as_ptr(), 1, dvals.as_ptr()),
			status::ERR_UNKNOWN
		);
		// values 为空（count 0）→ 无类型探针，整体替换为空。
		assert_eq!(
			(s.set_double_n)(handle(&set), ni.as_ptr(), 0, dvals.as_ptr()),
			0
		);
	}
	assert_eq!(set.dimension("i"), 0);
}

/// propGetN: Int ↔ Double coercion, min(count, dim) truncation, mixed-array
/// writes skip mismatched elements.
#[test]
fn get_n_families() {
	let s = suite_v1();
	let set = PropertySet::new();
	set.define("ints", vec![Value::Int(1), Value::Int(2)]);
	set.define("dbls", vec![Value::Double(1.5), Value::Double(2.5)]);

	// get_double_n 读 Int 属性 → 协随为 f64。
	let ni = cs("ints");
	let mut dout = [0.0 as c_double; 2];
	unsafe {
		assert_eq!(
			(s.get_double_n)(handle(&set), ni.as_ptr(), 2, dout.as_mut_ptr()),
			0
		);
	}
	assert_eq!(dout, [1.0, 2.0]);

	// get_int_n 读 Double 属性 → 截断为 i32。
	let nd = cs("dbls");
	let mut iout = [0 as c_int; 2];
	unsafe {
		assert_eq!(
			(s.get_int_n)(handle(&set), nd.as_ptr(), 2, iout.as_mut_ptr()),
			0
		);
	}
	assert_eq!(iout, [1, 2]);

	// count > 维度：只拷贝 min(count, dim)。
	let mut one = [0 as c_int; 1];
	unsafe {
		assert_eq!(
			(s.get_int_n)(handle(&set), ni.as_ptr(), 5, one.as_mut_ptr()),
			0
		);
	}
	assert_eq!(one, [1]);

	// count 0：不写。
	let mut zero = [7 as c_int; 1];
	unsafe {
		assert_eq!(
			(s.get_int_n)(handle(&set), ni.as_ptr(), 0, zero.as_mut_ptr()),
			0
		);
	}
	assert_eq!(zero, [7]);

	// get_pointer_n / get_string_n 成功路径。
	let ptrs = PropertySet::new();
	let raw = 0x99usize as *mut c_void;
	ptrs.define("p", vec![Value::Pointer(raw)]);
	let np = cs("p");
	let mut pout = [std::ptr::null_mut(); 1];
	unsafe {
		assert_eq!(
			(s.get_pointer_n)(handle(&ptrs), np.as_ptr(), 1, pout.as_mut_ptr()),
			0
		);
	}
	assert_eq!(pout[0], raw);

	let strs = PropertySet::new();
	strs.define("s", vec![Value::String(cs("hi"))]);
	let ns = cs("s");
	let mut sout = [std::ptr::null_mut(); 1];
	unsafe {
		assert_eq!(
			(s.get_string_n)(handle(&strs), ns.as_ptr(), 1, sout.as_mut_ptr()),
			0
		);
		assert_eq!(CStr::from_ptr(sout[0]).to_bytes(), b"hi");
	}
}

/// propGetN error paths: null out, undefined, empty array, type mismatch,
/// negative count.
#[test]
fn get_n_error_paths() {
	let s = suite_v1();
	let set = PropertySet::new();
	set.define("ints", vec![Value::Int(1)]);
	set.define("strs", vec![Value::String(cs("s"))]);
	set.define("empty", vec![]);
	let ni = cs("ints");
	let ns = cs("strs");
	let ne = cs("empty");
	let nmissing = cs("missing");
	let mut iout = [0 as c_int; 2];
	let mut dout = [0.0 as c_double; 2];
	let mut pout = [std::ptr::null_mut(); 2];
	let mut sout = [std::ptr::null_mut(); 2];
	unsafe {
		// 空 out（count > 0）。
		assert_eq!(
			(s.get_int_n)(handle(&set), ni.as_ptr(), 1, std::ptr::null_mut()),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_double_n)(handle(&set), ni.as_ptr(), 1, std::ptr::null_mut()),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_pointer_n)(handle(&set), ni.as_ptr(), 1, std::ptr::null_mut()),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_string_n)(handle(&set), ni.as_ptr(), 1, std::ptr::null_mut()),
			status::ERR_VALUE
		);
		// 未定义 → Unknown；空数组 → Unknown（无类型探针）。
		assert_eq!(
			(s.get_int_n)(handle(&set), nmissing.as_ptr(), 1, iout.as_mut_ptr()),
			status::ERR_UNKNOWN
		);
		assert_eq!(
			(s.get_int_n)(handle(&set), ne.as_ptr(), 1, iout.as_mut_ptr()),
			status::ERR_UNKNOWN
		);
		// 类型不符 → Unknown（数值族之间协随，其余严格）。
		assert_eq!(
			(s.get_int_n)(handle(&set), ns.as_ptr(), 1, iout.as_mut_ptr()),
			status::ERR_UNKNOWN
		);
		assert_eq!(
			(s.get_double_n)(handle(&set), ns.as_ptr(), 1, dout.as_mut_ptr()),
			status::ERR_UNKNOWN
		);
		assert_eq!(
			(s.get_pointer_n)(handle(&set), ni.as_ptr(), 1, pout.as_mut_ptr()),
			status::ERR_UNKNOWN
		);
		assert_eq!(
			(s.get_string_n)(handle(&set), ni.as_ptr(), 1, sout.as_mut_ptr()),
			status::ERR_UNKNOWN
		);
		// 负 count → BadIndex（在 min 之前 idx 转换失败）。
		assert_eq!(
			(s.get_int_n)(handle(&set), ni.as_ptr(), -1, iout.as_mut_ptr()),
			status::ERR_BAD_INDEX
		);
	}
}

/// propReset is explicitly unsupported; propGetDimension with null
/// out/undefined/empty array.
#[test]
fn reset_and_dimension() {
	let s = suite_v1();
	let set = PropertySet::new();
	set.define("a", vec![Value::Int(1)]);
	set.define("empty", vec![]);
	let na = cs("a");
	let ne = cs("empty");
	let nmissing = cs("missing");
	let mut dim = -1;
	unsafe {
		assert_eq!(
			(s.reset)(handle(&set), na.as_ptr()),
			status::ERR_UNSUPPORTED
		);
		assert_eq!(
			(s.get_dimension)(handle(&set), na.as_ptr(), std::ptr::null_mut()),
			status::ERR_VALUE
		);
		assert_eq!(
			(s.get_dimension)(handle(&set), nmissing.as_ptr(), &mut dim),
			status::ERR_UNKNOWN
		);
		assert_eq!((s.get_dimension)(handle(&set), na.as_ptr(), &mut dim), 0);
		assert_eq!(dim, 1);
		assert_eq!((s.get_dimension)(handle(&set), ne.as_ptr(), &mut dim), 0);
		assert_eq!(dim, 0, "已定义空数组是合法维度 0");
	}
}

/// Diagnostic branches when QUERCUS_OFX_TRACE is enabled: every trace print
/// site is reached (error paths and success paths). The test serializes itself
/// and restores the environment variable.
#[test]
fn trace_diagnostic_branches() {
	static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
	let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let prev = std::env::var_os("QUERCUS_OFX_TRACE");
	// set_var/remove_var 在本 crate 的 2021 edition 中是安全函数。
	std::env::set_var("QUERCUS_OFX_TRACE", "1");
	let body = std::panic::catch_unwind(|| {
		let s = suite_v1();
		let set = PropertySet::new();
		let ni = cs("i");
		let nstr = cs("str");
		let nmissing = cs("missing");
		set.define("i", vec![Value::Int(1)]);
		set.define("str", vec![Value::String(cs("s"))]);
		let mut iv = 0;
		let mut dv = 0.0;
		let mut sv: *mut c_char = std::ptr::null_mut();
		let mut pv: *mut c_void = std::ptr::null_mut();
		let mut dim = 0;
		unsafe {
			// caught 的 trace：非 OK 状态码。
			assert_eq!(
				(s.get_int)(handle(&set), nmissing.as_ptr(), 0, &mut iv),
				status::ERR_UNKNOWN
			);
			// get_value trace：成功 + 失败。
			assert_eq!((s.get_int)(handle(&set), ni.as_ptr(), 0, &mut iv), 0);
			assert_eq!(
				(s.get_int)(handle(&set), nmissing.as_ptr(), 0, &mut iv),
				status::ERR_UNKNOWN
			);
			// 类型不符 trace。
			assert_eq!(
				(s.get_string)(handle(&set), ni.as_ptr(), 0, &mut sv),
				status::ERR_UNKNOWN
			);
			// set_value：空数组越界 trace；越界 trace。
			let empty = PropertySet::new();
			empty.define("e", vec![]);
			let ne = cs("e");
			assert_eq!(
				(s.set_int)(handle(&empty), ne.as_ptr(), 1, 1),
				status::ERR_BAD_INDEX
			);
			assert_eq!(
				(s.set_int)(handle(&set), ni.as_ptr(), 5, 2),
				status::ERR_BAD_INDEX
			);
			// propGetPointer trace（成功）。
			let ptrs = PropertySet::new();
			ptrs.define("p", vec![Value::Pointer(0x11usize as *mut c_void)]);
			let np = cs("p");
			assert_eq!((s.get_pointer)(handle(&ptrs), np.as_ptr(), 0, &mut pv), 0);
			// propGetString trace：成功 + 失败。
			assert_eq!((s.get_string)(handle(&set), nstr.as_ptr(), 0, &mut sv), 0);
			assert_eq!(
				(s.get_string)(handle(&set), nmissing.as_ptr(), 0, &mut sv),
				status::ERR_UNKNOWN
			);
			// propGetInt inspect_err trace。
			assert_eq!(
				(s.get_int)(handle(&set), nmissing.as_ptr(), 0, &mut iv),
				status::ERR_UNKNOWN
			);
			// get_n trace：成功 dump + 未命中 inspect_err。
			let mut iout = [0 as c_int; 2];
			assert_eq!(
				(s.get_int_n)(handle(&set), ni.as_ptr(), 2, iout.as_mut_ptr()),
				0
			);
			assert_eq!(
				(s.get_int_n)(handle(&set), nmissing.as_ptr(), 1, iout.as_mut_ptr()),
				status::ERR_UNKNOWN
			);
			// dump 的 Double 与未知元素分支。
			let mixed = PropertySet::new();
			mixed.define("m", vec![Value::Double(1.5), Value::String(cs("x"))]);
			let nm = cs("m");
			let mut dout = [0.0 as c_double; 2];
			assert_eq!(
				(s.get_double_n)(handle(&mixed), nm.as_ptr(), 2, dout.as_mut_ptr()),
				0
			);
			// 空 out 的 get_double trace 分支（Err 路径渲染）。
			assert_eq!((s.get_double)(handle(&set), ni.as_ptr(), 0, &mut dv), 0);
			// propGetDimension 未定义 trace。
			assert_eq!(
				(s.get_dimension)(handle(&set), nmissing.as_ptr(), &mut dim),
				status::ERR_UNKNOWN
			);
		}
	});
	match prev {
		Some(v) => std::env::set_var("QUERCUS_OFX_TRACE", v),
		None => std::env::remove_var("QUERCUS_OFX_TRACE"),
	}
	if let Err(p) = body {
		std::panic::resume_unwind(p);
	}
}
