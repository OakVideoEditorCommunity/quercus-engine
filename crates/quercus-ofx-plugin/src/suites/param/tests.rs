use super::*;
use std::ffi::CString;
use std::sync::Arc;

use crate::host::Plugin;
use crate::param::{ParamInstance, ParamSetInstance};
use crate::property::PropertySet;

fn cs(s: &str) -> CString {
	CString::new(s).unwrap()
}

fn descriptor_handle(d: &EffectDescriptor) -> *mut c_void {
	tag::make(
		&d.props as *const crate::property::PropertySet,
		tag::DESCRIPTOR,
	)
}

/// Fake plugin (host::Plugin is only constructed to obtain an Arc to feed the
/// Instance; fields other than the descriptor are not touched by this test).
fn dummy_plugin(descriptor: EffectDescriptor) -> Arc<Plugin> {
	unsafe extern "C" fn dummy_entry(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		status::OK
	}
	Arc::new(Plugin {
		identifier: "test.plugin".into(),
		version: (1, 0),
		bundle_path: std::path::PathBuf::new(),
		contexts: vec![],
		descriptor,
		lib: std::ptr::null_mut(),
		entry: dummy_entry,
		ofx_plugin: std::ptr::null_mut(),
		unloaded: std::sync::atomic::AtomicBool::new(false),
	})
}

fn instance_handle(i: &Instance) -> *mut c_void {
	tag::make(
		&i.props as *const crate::property::PropertySet,
		tag::INSTANCE,
	)
}

/// Build an instance through paramDefine (describe output -> createInstance).
fn make_instance() -> (Arc<Instance>, *mut c_void) {
	let desc = EffectDescriptor::new();
	let s = suite_v1();
	let dhandle = descriptor_handle(&desc);
	unsafe {
		for (t, n) in [
			("OfxParamTypeInteger", "gain"),
			("OfxParamTypeDouble", "opacity"),
			("OfxParamTypeDouble2D", "pos"),
			("OfxParamTypeRGB", "color"),
			("OfxParamTypeBoolean", "enabled"),
			("OfxParamTypeString", "label"),
		] {
			let t = cs(t);
			let n = cs(n);
			let mut ph: *mut c_void = std::ptr::null_mut();
			assert_eq!(
				(s.param_define)(dhandle, t.as_ptr(), n.as_ptr(), &mut ph),
				0
			);
		}
	}
	let params = ParamSetInstance {
		params: desc
			.params
			.iter()
			.map(|d| Box::new(ParamInstance::from_def((**d).clone())))
			.collect(),
	};
	let plugin = dummy_plugin(desc);
	let inst = Arc::new(Instance {
		props: PropertySet::new(),
		plugin,
		context: "OfxImageEffectContextFilter".into(),
		params,
		clips: vec![],
		node_identity: std::sync::atomic::AtomicUsize::new(0),
		destroyed: std::sync::atomic::AtomicBool::new(false),
		sequence_range: std::sync::Mutex::new(None),
		progress_cb: std::sync::Mutex::new(None),
		cancel: std::sync::atomic::AtomicBool::new(false),
		edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
		render_lock: std::sync::Mutex::new(()),
		interact: std::sync::Mutex::new(None),
	});
	(inst.clone(), instance_handle(&inst))
}

#[test]
fn describe_define_and_get() {
	let desc = EffectDescriptor::new();
	let s = suite_v1();
	let handle = descriptor_handle(&desc);
	let t = cs("OfxParamTypeDouble");
	let n = cs("opacity");

	let mut ph: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!((s.param_define)(handle, t.as_ptr(), n.as_ptr(), &mut ph), 0);
	}
	assert!(!ph.is_null());
	assert_eq!(tag::kind(ph), tag::PARAM_DEF);

	// paramGetValue during describe: default value 0.0.
	let mut v = 99.0;
	unsafe {
		assert_eq!((s.param_get_value)(ph, &mut v), 0);
	}
	assert_eq!(v, 0.0);

	// set during describe → BadHandle (HS: verifyMagic fails for a descriptor).
	unsafe {
		assert_eq!((s.param_set_value)(ph, 42.0), status::ERR_BAD_HANDLE);
	}

	// paramGetHandle by name (props handle has the same value).
	let mut ph2: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.param_get_handle)(handle, n.as_ptr(), &mut ph2, std::ptr::null_mut()),
			0
		);
		assert_eq!(ph, ph2);
	}

	// Not found → ErrUnknown (HS:1758).
	let nope = cs("nope");
	unsafe {
		assert_eq!(
			(s.param_get_handle)(handle, nope.as_ptr(), &mut ph2, std::ptr::null_mut()),
			status::ERR_UNKNOWN
		);
	}

	// Undefined type → ErrUnsupported (HS:1704); parametric → allowed
	// (curve read/write goes through OfxParametricParameterSuite).
	let bad = cs("OfxParamTypeBogus");
	let par = cs("OfxParamTypeParametric");
	unsafe {
		assert_eq!(
			(s.param_define)(handle, bad.as_ptr(), n.as_ptr(), &mut ph2),
			status::ERR_UNSUPPORTED
		);
		assert_eq!(
			(s.param_define)(handle, par.as_ptr(), n.as_ptr(), &mut ph2),
			0
		);
	}
	assert_eq!(tag::kind(ph2), tag::PARAM_DEF);

	// paramDefine during instance phase → BadHandle.
	let (_inst, ih) = make_instance();
	unsafe {
		assert_eq!(
			(s.param_define)(ih, t.as_ptr(), n.as_ptr(), &mut ph2),
			status::ERR_BAD_HANDLE
		);
	}
}

#[test]
fn instance_get_set_variadic_roundtrip() {
	let (_inst, ih) = make_instance();
	let s = suite_v1();

	// Grab each parameter's handle.
	let mut gain: *mut c_void = std::ptr::null_mut();
	let mut opacity: *mut c_void = std::ptr::null_mut();
	let mut pos: *mut c_void = std::ptr::null_mut();
	let mut color: *mut c_void = std::ptr::null_mut();
	let mut enabled: *mut c_void = std::ptr::null_mut();
	let mut label: *mut c_void = std::ptr::null_mut();
	unsafe {
		for (n, out) in [
			("gain", &mut gain),
			("opacity", &mut opacity),
			("pos", &mut pos),
			("color", &mut color),
			("enabled", &mut enabled),
			("label", &mut label),
		] {
			let n = cs(n);
			let r = (s.param_get_handle)(ih, n.as_ptr(), out, std::ptr::null_mut());
			assert_eq!(r, 0, "gethandle {} failed: {}", n.to_str().unwrap(), r);
			assert_eq!(tag::kind(*out), tag::PARAM_INSTANCE);
		}
	}

	// Integer set/get.
	unsafe {
		assert_eq!((s.param_set_value)(gain, 7), 0);
	}
	let mut v = 0;
	unsafe {
		assert_eq!((s.param_get_value)(gain, &mut v), 0);
	}
	assert_eq!(v, 7);

	// Double2D.
	unsafe {
		assert_eq!((s.param_set_value)(pos, 1.5, -2.5), 0);
	}
	let (mut x, mut y) = (0.0, 0.0);
	unsafe {
		assert_eq!((s.param_get_value)(pos, &mut x, &mut y), 0);
	}
	assert_eq!((x, y), (1.5, -2.5));

	// RGB.
	unsafe {
		assert_eq!((s.param_set_value)(color, 0.1, 0.2, 0.3), 0);
	}
	let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
	unsafe {
		assert_eq!((s.param_get_value)(color, &mut r, &mut g, &mut b), 0);
	}
	assert_eq!((r, g, b), (0.1, 0.2, 0.3));

	// Boolean (int 0/1).
	unsafe {
		assert_eq!((s.param_set_value)(enabled, 1), 0);
	}
	let mut e = 0;
	unsafe {
		assert_eq!((s.param_get_value)(enabled, &mut e), 0);
	}
	assert_eq!(e, 1);

	// String: after set, get returns the interior pointer.
	let hello = cs("hello");
	unsafe {
		assert_eq!((s.param_set_value)(label, hello.as_ptr()), 0);
	}
	let mut p: *mut c_char = std::ptr::null_mut();
	unsafe {
		assert_eq!((s.param_get_value)(label, &mut p), 0);
		assert_eq!(CStr::from_ptr(p).to_bytes(), b"hello");
	}

	// AtTime == current value (no animation).
	let mut v2 = 0.0;
	unsafe {
		assert_eq!((s.param_get_value_at_time)(opacity, 12.0, &mut v2), 0);
		assert_eq!(v2, 0.0);
		assert_eq!((s.param_set_value_at_time)(opacity, 12.0, 0.75), 0);
	}
	let mut v3 = 0.0;
	unsafe {
		assert_eq!((s.param_get_value)(opacity, &mut v3), 0);
	}
	assert_eq!(v3, 0.75);

	// keys: always 0 / BadIndex.
	let mut nkeys = -1;
	unsafe {
		assert_eq!((s.param_get_num_keys)(opacity, &mut nkeys), 0);
		assert_eq!(nkeys, 0);
		let mut kt = 0.0;
		assert_eq!(
			(s.param_get_key_time)(opacity, 0, &mut kt),
			status::ERR_BAD_INDEX
		);
	}

	// derivative/integral → MissingHostFeature.
	let mut dv = 0.0;
	unsafe {
		assert_eq!(
			(s.param_get_derivative)(opacity, 1.0, &mut dv),
			status::ERR_MISSING_HOST_FEATURE
		);
	}

	// editBegin/End, deleteKey(s), copy: no-op OK.
	unsafe {
		assert_eq!((s.param_edit_begin)(opacity), 0);
		assert_eq!((s.param_edit_end)(opacity), 0);
		assert_eq!((s.param_delete_key)(opacity, 1.0), 0);
		assert_eq!((s.param_delete_all_keys)(opacity), 0);
		assert_eq!(
			(s.param_copy)(opacity, opacity, 1.0, 1.0, std::ptr::null()),
			0
		);
	}
}

/// paramSetValue -> instanceChanged write-back (C++ paramChangedByPlugin):
/// once the instance is bound to a node, a plugin-side value change locates
/// the instance through PARAM_OWNER, notify_instance_changed produces an undo
/// command and immediately redoes it — the node input is the new value.
#[test]
fn set_value_writes_back_to_bound_node() {
	let (inst, ih) = make_instance();
	let s = suite_v1();
	let mut gain: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.param_get_handle)(ih, cs("gain").as_ptr(), &mut gain, std::ptr::null_mut()),
			0
		);
	}

	// Host-side registration: param → instance (production does this in the createInstance path),
	// instance → quercusnode node (done at facade assembly time).
	let project = quercus_node::project::Project::new();
	let node_id = {
		let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
		let id = guard.graph.add_node(
			quercus_node::node::NodeCore::empty(),
			Box::new(quercus_node::nodes::EmptyBehavior),
		);
		guard
			.graph
			.get_mut(id)
			.unwrap()
			.core
			.add_input(quercus_node::input::Input::new(
				"gain",
				quercus_node::value::ValueType::Int,
				quercus_node::value::NodeValue::Int(0),
			));
		id
	};
	let identity = crate::node::register_node(project.clone(), node_id);
	inst.bind_node(identity as usize);
	let param_addr = tag::strip(gain) as usize;
	register_param_owner(param_addr, &inst.props as *const PropertySet as usize);

	unsafe {
		assert_eq!((s.param_set_value)(gain, 5), 0);
	}
	let stored = project
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.graph
		.get(node_id)
		.unwrap()
		.core
		.standard_value("gain", -1);
	assert_eq!(
		stored,
		quercus_node::value::NodeValue::Int(5),
		"plugin value change must write back to node input via instanceChanged"
	);
	unregister_params_of(&inst.props as *const PropertySet as usize);
	crate::node::unregister_node(identity);
}
