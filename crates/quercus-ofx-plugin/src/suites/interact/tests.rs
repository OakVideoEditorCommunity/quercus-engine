use super::*;
use std::ffi::{c_char, CStr};
use std::sync::Mutex;

/// The interact property table: PixelScale/ViewportSize/SuggestedColour/
/// SlaveToParam/BitDepth/HasAlpha etc. are preset per the header and
/// readable/writable through the property suite.
#[test]
fn interact_props_initialized() {
	let props = PropertySet::new();
	init_interact_props(&props);
	// Value 无 PartialEq（指针/CString 成员），用 matches! 断言。
	assert!(
		matches!(
			props.get(crate::host::PROP_INTERACT_PIXEL_SCALE, 0),
			Some(Value::Double(1.0))
		),
		"PixelScale[0] 应为 1.0"
	);
	assert_eq!(props.dimension(crate::host::PROP_INTERACT_PIXEL_SCALE), 2);
	assert!(
		matches!(
			props.get(crate::host::PROP_INTERACT_SUGGESTED_COLOUR, 2),
			Some(Value::Double(1.0))
		),
		"SuggestedColour[2] 应为 1.0"
	);
	assert_eq!(
		props.dimension(crate::host::PROP_INTERACT_SLAVE_TO_PARAM),
		0
	);
	assert!(
		matches!(
			props.get(crate::host::PROP_INTERACT_BIT_DEPTH, 0),
			Some(Value::Int(32))
		),
		"BitDepth 应为 32"
	);
	assert!(
		matches!(
			props.get(crate::host::PROP_INTERACT_HAS_ALPHA, 0),
			Some(Value::Int(1))
		),
		"HasAlpha 应为 1"
	);
}

/// read_double2 helper function.
#[test]
fn read_double2_helper() {
	let props = PropertySet::new();
	props.define("P", vec![Value::Double(2.0), Value::Double(3.0)]);
	assert_eq!(read_double2(&props, "P"), Some((2.0, 3.0)));
	assert_eq!(read_double2(&props, "Q"), None);
}

/// Null pointer / wrong tag → ErrBadHandle (the interact suite's handle
/// validation).
#[test]
fn interact_suite_handle_validation() {
	unsafe {
		assert_eq!(
			interact_swap_buffers(std::ptr::null_mut()),
			status::ERR_BAD_HANDLE
		);
		assert_eq!(
			interact_redraw(std::ptr::null_mut()),
			status::ERR_BAD_HANDLE
		);
		let mut out: *mut c_void = std::ptr::null_mut();
		assert_eq!(
			interact_get_property_set(std::ptr::null_mut(), &mut out),
			status::ERR_BAD_HANDLE
		);
		// 错标签：裸 PropertySet（标签 0）当 interact 用。
		let props = PropertySet::new();
		let raw = &props as *const PropertySet as *mut c_void;
		assert_eq!(interact_swap_buffers(raw), status::ERR_BAD_HANDLE);
	}
}

/// interact suite server side: swap/redraw requests are recorded (the
/// app-side polling surface) and getPropertySet returns the interact property
/// set handle.
#[test]
fn interact_suite_requests_are_recorded() {
	// 用假插件构造 Interact（不进插件入口——只测 suite 服务端）。
	let plugin = crate::host::Plugin {
		identifier: "fake".into(),
		version: (1, 0),
		bundle_path: std::path::PathBuf::from("fake"),
		contexts: Vec::new(),
		descriptor: crate::descriptor::EffectDescriptor::new(),
		lib: std::ptr::null_mut(),
		entry: dummy_entry,
		ofx_plugin: std::ptr::null_mut(),
		unloaded: std::sync::atomic::AtomicBool::new(false),
	};
	let interact = Interact::new(Arc::new(plugin), dummy_entry, std::ptr::null_mut());
	let handle = interact.handle();

	unsafe {
		assert_eq!(interact_swap_buffers(handle), status::OK);
		assert_eq!(interact_redraw(handle), status::OK);
	}
	assert!(interact.swap_requested.load(Ordering::Relaxed));
	assert!(interact.redraw_requested.load(Ordering::Relaxed));

	let mut props_handle: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			interact_get_property_set(handle, &mut props_handle),
			status::OK
		);
	}
	assert_eq!(tag::kind(props_handle), tag::INTERACT);
	assert_eq!(
		tag::strip(props_handle) as usize,
		&interact.props as *const PropertySet as usize
	);

	// property suite 可直读 interact 属性集（句柄约定打通）。
	let addr = tag::strip(props_handle);
	let set: &PropertySet = unsafe { &*addr };
	assert!(
		matches!(
			set.get(crate::host::PROP_INTERACT_BIT_DEPTH, 0),
			Some(Value::Int(32))
		),
		"经 interact 句柄的 property suite 应读到 BitDepth=32"
	);

	// 摘除引用计数：Arc 释放（interact 已无引用）。
	drop(interact);
}

/// Fake entry: every action returns ReplyDefault (never reaches a real
/// plugin).
unsafe extern "C" fn dummy_entry(
	_action: *const c_char,
	_handle: *const c_void,
	_in: *mut c_void,
	_out: *mut c_void,
) -> c_int {
	status::REPLY_DEFAULT
}

/// call-side pass-through: the entry receives the action and the tagged
/// handle (recorded by the fake plugin).
#[test]
fn interact_call_dispatches_to_entry() {
	static CALLED_ACTION: Mutex<Vec<String>> = Mutex::new(Vec::new());
	unsafe extern "C" fn recording_entry(
		action: *const c_char,
		handle: *const c_void,
		_in: *mut c_void,
		_out: *mut c_void,
	) -> c_int {
		CALLED_ACTION.lock().unwrap().push(
			unsafe { CStr::from_ptr(action) }
				.to_string_lossy()
				.into_owned(),
		);
		// handle 必须是 tagged INTERACT。
		assert_eq!(tag::kind(handle as *mut c_void), tag::INTERACT);
		status::OK
	}
	let plugin = crate::host::Plugin {
		identifier: "fake".into(),
		version: (1, 0),
		bundle_path: std::path::PathBuf::from("fake"),
		contexts: Vec::new(),
		descriptor: crate::descriptor::EffectDescriptor::new(),
		lib: std::ptr::null_mut(),
		entry: recording_entry,
		ofx_plugin: std::ptr::null_mut(),
		unloaded: std::sync::atomic::AtomicBool::new(false),
	};
	let interact = Interact::new(Arc::new(plugin), recording_entry, std::ptr::null_mut());

	assert_eq!(interact.describe(), status::OK);
	assert_eq!(interact.create_instance(), status::OK);
	assert_eq!(interact.pen_motion((10.0, 20.0), true, 5.0), status::OK);
	assert_eq!(interact.pen_down((10.0, 20.0), 5.0), status::OK);
	assert_eq!(interact.pen_up((10.0, 20.0), 5.0), status::OK);
	assert_eq!(interact.key_down(crate::host::KEY_A, "a", 5.0), status::OK);
	assert_eq!(interact.key_up(crate::host::KEY_A, "a", 5.0), status::OK);
	assert_eq!(interact.idle(), status::OK);
	interact.destroy();

	let calls = CALLED_ACTION.lock().unwrap().clone();
	let want = [
		crate::host::ACTION_DESCRIBE,
		crate::host::ACTION_CREATE_INSTANCE,
		crate::host::ACTION_INTERACT_PEN_MOTION,
		crate::host::ACTION_INTERACT_PEN_DOWN,
		crate::host::ACTION_INTERACT_PEN_UP,
		crate::host::ACTION_INTERACT_KEY_DOWN,
		crate::host::ACTION_INTERACT_KEY_UP,
		crate::host::ACTION_INTERACT_IDLE,
		crate::host::ACTION_DESTROY_INSTANCE,
	];
	assert_eq!(calls, want, "interact action 序列应按序透传到入口");
}

/// Real pen event parameters: the inArgs canonical position / viewport
/// position / pressure are converted through pixelScale (read and recorded by
/// the fake plugin).
#[test]
fn pen_in_args_canonical_conversion() {
	// 记录 pen_motion 的 inArgs 关键属性。
	static CAPTURED: Mutex<Option<(f64, f64, f64)>> = Mutex::new(None);
	unsafe extern "C" fn capture_entry(
		_action: *const c_char,
		_handle: *const c_void,
		in_args: *mut c_void,
		_out: *mut c_void,
	) -> c_int {
		if in_args.is_null() {
			return status::OK;
		}
		let set = unsafe { &*(in_args as *const PropertySet) };
		// 视口位置（Int×2）与 canonical 位置（Double×2）与压力。
		let vx = match set.get(crate::host::PROP_INTERACT_PEN_VIEWPORT_POSITION, 0) {
			Some(Value::Int(i)) => i,
			_ => 0,
		};
		let px = match set.get(crate::host::PROP_INTERACT_PEN_POSITION, 0) {
			Some(Value::Double(d)) => d,
			_ => 0.0,
		};
		let pressure = match set.get(crate::host::PROP_INTERACT_PEN_PRESSURE, 0) {
			Some(Value::Double(d)) => d,
			_ => 0.0,
		};
		*CAPTURED.lock().unwrap() = Some((vx as f64, px, pressure));
		status::OK
	}
	let plugin = crate::host::Plugin {
		identifier: "fake".into(),
		version: (1, 0),
		bundle_path: std::path::PathBuf::from("fake"),
		contexts: Vec::new(),
		descriptor: crate::descriptor::EffectDescriptor::new(),
		lib: std::ptr::null_mut(),
		entry: capture_entry,
		ofx_plugin: std::ptr::null_mut(),
		unloaded: std::sync::atomic::AtomicBool::new(false),
	};
	let interact = Interact::new(Arc::new(plugin), capture_entry, std::ptr::null_mut());
	// 设 pixelScale=2 → canonical = viewport/2。
	interact.props.define(
		crate::host::PROP_INTERACT_PIXEL_SCALE,
		vec![Value::Double(2.0), Value::Double(2.0)],
	);

	assert_eq!(interact.pen_motion((10.0, 20.0), true, 0.0), status::OK);
	let (vx, px, pressure) = CAPTURED.lock().unwrap().take().unwrap();
	assert_eq!(vx, 10.0);
	assert_eq!(px, 5.0);
	assert_eq!(pressure, 1.0);

	// pen_up 压力 0。
	assert_eq!(interact.pen_up((10.0, 20.0), 0.0), status::OK);
	let (_, _, pressure) = CAPTURED.lock().unwrap().take().unwrap();
	assert_eq!(pressure, 0.0);
}

/// Real key event parameters: keySym/keyString enter inArgs.
#[test]
fn key_in_args_carry_sym_and_string() {
	static CAPTURED: Mutex<Option<(i32, String)>> = Mutex::new(None);
	unsafe extern "C" fn capture_entry(
		_action: *const c_char,
		_handle: *const c_void,
		in_args: *mut c_void,
		_out: *mut c_void,
	) -> c_int {
		let set = unsafe { &*(in_args as *const PropertySet) };
		let sym = match set.get(crate::host::PROP_KEY_SYM, 0) {
			Some(Value::Int(i)) => i,
			_ => 0,
		};
		let s = match set.get(crate::host::PROP_KEY_STRING, 0) {
			Some(Value::String(c)) => c.to_string_lossy().into_owned(),
			_ => String::new(),
		};
		*CAPTURED.lock().unwrap() = Some((sym, s));
		status::OK
	}
	let plugin = crate::host::Plugin {
		identifier: "fake".into(),
		version: (1, 0),
		bundle_path: std::path::PathBuf::from("fake"),
		contexts: Vec::new(),
		descriptor: crate::descriptor::EffectDescriptor::new(),
		lib: std::ptr::null_mut(),
		entry: capture_entry,
		ofx_plugin: std::ptr::null_mut(),
		unloaded: std::sync::atomic::AtomicBool::new(false),
	};
	let interact = Interact::new(Arc::new(plugin), capture_entry, std::ptr::null_mut());
	assert_eq!(
		interact.key_down(crate::host::KEY_RETURN, "", 1.0),
		status::OK
	);
	let (sym, s) = CAPTURED.lock().unwrap().take().unwrap();
	assert_eq!(sym, crate::host::KEY_RETURN);
	assert_eq!(s, "");
	assert_eq!(interact.key_up(crate::host::KEY_A, "a", 1.0), status::OK);
	let (sym, s) = CAPTURED.lock().unwrap().take().unwrap();
	assert_eq!(sym, crate::host::KEY_A);
	assert_eq!(s, "a");
}

/// destroy is idempotent: it is sent only once.
#[test]
fn destroy_is_idempotent() {
	static COUNT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
	unsafe extern "C" fn counting_entry(
		action: *const c_char,
		_handle: *const c_void,
		_in: *mut c_void,
		_out: *mut c_void,
	) -> c_int {
		let a = unsafe { CStr::from_ptr(action) }.to_string_lossy();
		if a == crate::host::ACTION_DESTROY_INSTANCE {
			COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		}
		status::OK
	}
	let plugin = crate::host::Plugin {
		identifier: "fake".into(),
		version: (1, 0),
		bundle_path: std::path::PathBuf::from("fake"),
		contexts: Vec::new(),
		descriptor: crate::descriptor::EffectDescriptor::new(),
		lib: std::ptr::null_mut(),
		entry: counting_entry,
		ofx_plugin: std::ptr::null_mut(),
		unloaded: std::sync::atomic::AtomicBool::new(false),
	};
	let interact = Interact::new(Arc::new(plugin), counting_entry, std::ptr::null_mut());
	interact.destroy();
	interact.destroy();
	assert_eq!(COUNT.load(std::sync::atomic::Ordering::Relaxed), 1);
}
