use super::*;
use std::ffi::{c_char, CString};
use std::sync::Arc;

use crate::descriptor::EffectDescriptor;
use crate::host::Plugin;
use crate::instance::Instance;
use crate::param::{ParamInstance, ParamSetInstance};
use crate::property::PropertySet;

fn cs(s: &str) -> CString {
	CString::new(s).unwrap()
}

fn descriptor_handle(d: &EffectDescriptor) -> *mut c_void {
	tag::make(&d.props as *const PropertySet, tag::DESCRIPTOR)
}

/// Fake plugin (a host::Plugin is constructed only to feed an Arc to
/// Instance; fields other than describe are not touched by this test).
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
	tag::make(&i.props as *const PropertySet, tag::INSTANCE)
}

/// Define a parametric parameter "curve" during describe (default
/// dimension 1); returns the descriptor and its param handle.
fn define_parametric() -> (EffectDescriptor, *mut c_void) {
	let desc = EffectDescriptor::new();
	let s = crate::suites::param::suite_v1();
	let dhandle = descriptor_handle(&desc);
	let t = cs(crate::param::TYPE_PARAMETRIC);
	let n = cs("curve");
	let mut ph: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.param_define)(dhandle, t.as_ptr(), n.as_ptr(), &mut ph),
			0
		);
	}
	assert_eq!(tag::kind(ph), tag::PARAM_DEF);
	(desc, ph)
}

/// Instantiate the describe product (copy default → instance value);
/// returns (instance Arc, instance handle, instance param handle).
fn instantiate(desc: EffectDescriptor) -> (Arc<Instance>, *mut c_void, *mut c_void) {
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
	let ih = instance_handle(&inst);
	let s = crate::suites::param::suite_v1();
	let n = cs("curve");
	let mut ph: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.param_get_handle)(ih, n.as_ptr(), &mut ph, std::ptr::null_mut()),
			0
		);
	}
	assert_eq!(tag::kind(ph), tag::PARAM_INSTANCE);
	(inst, ih, ph)
}

/// Define during describe and instantiate in one step (default dimension
/// 1).
fn make_instance() -> (Arc<Instance>, *mut c_void, *mut c_void) {
	let (desc, _dph) = define_parametric();
	instantiate(desc)
}

fn ps() -> &'static ParametricParameterSuiteV1 {
	suite_v1()
}

/// Describe-phase defaults: 2 identity control points (0,0)/(1,1);
/// evaluation = identity.
#[test]
fn describe_defaults_are_identity() {
	let (_desc, ph) = define_parametric();
	let s = ps();
	let mut n = -1;
	unsafe {
		assert_eq!(
			(s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n),
			0
		);
	}
	assert_eq!(n, 2);
	let (mut k, mut v) = (-1.0, -1.0);
	unsafe {
		assert_eq!(
			(s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 0, &mut k, &mut v),
			0
		);
	}
	assert_eq!((k, v), (0.0, 0.0));
	unsafe {
		assert_eq!(
			(s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 1, &mut k, &mut v),
			0
		);
	}
	assert_eq!((k, v), (1.0, 1.0));
	// 恒等求值（越界钳制端点值）。
	let mut out = 0.0;
	unsafe {
		assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.5, &mut out), 0);
	}
	assert_eq!(out, 0.5);
	unsafe {
		assert_eq!(
			(s.parametric_param_get_value)(ph, 0, 0.0, -3.0, &mut out),
			0
		);
		assert_eq!(out, 0.0);
		assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 7.0, &mut out), 0);
		assert_eq!(out, 1.0);
	}
}

/// Set/Add/Delete during describe change the **default value**, copied to
/// the instance at instantiation.
#[test]
fn descriptor_edits_become_instance_defaults() {
	let (desc, ph) = define_parametric();
	let s = ps();
	// describe 期改默认：加两个点，改第 1 个点的值。
	unsafe {
		assert_eq!(
			(s.parametric_param_add_control_point)(ph, 0, 0.0, 0.25, 0.5, 0),
			0
		);
		assert_eq!(
			(s.parametric_param_add_control_point)(ph, 0, 0.0, 0.75, 0.9, 0),
			0
		);
		assert_eq!(
			(s.parametric_param_set_nth_control_point)(ph, 0, 0.0, 1, 0.25, 0.6, 0),
			0
		);
	}
	// 默认值本身生效。
	let mut out = 0.0;
	unsafe {
		assert_eq!(
			(s.parametric_param_get_value)(ph, 0, 0.0, 0.25, &mut out),
			0
		);
	}
	assert_eq!(out, 0.6);
	// 实例化：默认曲线随 from_def 复制。
	let (_inst, _ih, iph) = instantiate(desc);
	unsafe {
		assert_eq!(
			(s.parametric_param_get_value)(iph, 0, 0.0, 0.25, &mut out),
			0
		);
	}
	assert_eq!(out, 0.6);
	let mut n = 0;
	unsafe {
		assert_eq!(
			(s.parametric_param_get_n_control_points)(iph, 0, 0.0, &mut n),
			0
		);
	}
	assert_eq!(n, 4); // (0,0) (0.25,0.6) (0.75,0.9) (1,1)
}

/// Full instance-phase chain: Add → Set → Delete → DeleteAll, with
/// evaluation following the model.
#[test]
fn instance_add_set_delete_eval_chain() {
	let (_inst, _ih, ph) = make_instance();
	let s = ps();

	// Add 中点 (0.3, 0.7)：插值性质 + 有序。
	unsafe {
		assert_eq!(
			(s.parametric_param_add_control_point)(ph, 0, 0.0, 0.3, 0.7, 0),
			0
		);
	}
	let mut out = 0.0;
	unsafe {
		assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.3, &mut out), 0);
	}
	assert_eq!(out, 0.7);
	let mut n = 0;
	unsafe {
		assert_eq!(
			(s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n),
			0
		);
	}
	assert_eq!(n, 3);

	// SetNth 改 (0.3,0.7) → (0.4,0.6)。
	unsafe {
		assert_eq!(
			(s.parametric_param_set_nth_control_point)(ph, 0, 0.0, 1, 0.4, 0.6, 0),
			0
		);
	}
	let (mut k, mut v) = (-1.0, -1.0);
	unsafe {
		assert_eq!(
			(s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 1, &mut k, &mut v),
			0
		);
	}
	assert_eq!((k, v), (0.4, 0.6));
	unsafe {
		assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.4, &mut out), 0);
	}
	assert_eq!(out, 0.6);

	// Delete 第 1 点 → 回到双点。
	unsafe {
		assert_eq!((s.parametric_param_delete_control_point)(ph, 0, 1), 0);
	}
	unsafe {
		assert_eq!(
			(s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n),
			0
		);
	}
	assert_eq!(n, 2);

	// DeleteAll → 0 点，求值退化为恒等。
	unsafe {
		assert_eq!((s.parametric_param_delete_all_control_points)(ph, 0), 0);
		assert_eq!(
			(s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n),
			0
		);
		assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.5, &mut out), 0);
	}
	assert_eq!(n, 0);
	assert_eq!(out, 0.5);
}

/// SetNth with a changed key breaks ordering → automatic re-sort (the
/// semantics the header warns about).
#[test]
fn set_nth_reorders_on_key_change() {
	let (_inst, _ih, ph) = make_instance();
	let s = ps();
	unsafe {
		assert_eq!(
			(s.parametric_param_add_control_point)(ph, 0, 0.0, 0.5, 0.5, 0),
			0
		);
		// 把第 0 点 (0,0) 挪到 0.75 → 序列变 (0.5) (0.75) (1)。
		assert_eq!(
			(s.parametric_param_set_nth_control_point)(ph, 0, 0.0, 0, 0.75, 0.75, 0),
			0
		);
	}
	let (mut k, mut v) = (-1.0, -1.0);
	for (i, (ek, ev)) in [(0.5, 0.5), (0.75, 0.75), (1.0, 1.0)].iter().enumerate() {
		unsafe {
			assert_eq!(
				(s.parametric_param_get_nth_control_point)(ph, 0, 0.0, i as c_int, &mut k, &mut v),
				0
			);
		}
		assert_eq!((k, v), (*ek, *ev), "nth {i}");
	}
}

/// Add with the same key → overwrite (no new point).
#[test]
fn add_overwrites_same_key() {
	let (_inst, _ih, ph) = make_instance();
	let s = ps();
	unsafe {
		assert_eq!(
			(s.parametric_param_add_control_point)(ph, 0, 0.0, 0.5, 0.4, 0),
			0
		);
		assert_eq!(
			(s.parametric_param_add_control_point)(ph, 0, 0.0, 0.5, 0.9, 0),
			0
		);
	}
	let mut n = 0;
	unsafe {
		assert_eq!(
			(s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n),
			0
		);
	}
	assert_eq!(n, 3); // (0,0) (0.5,0.9) (1,1)
	let (mut k, mut v) = (-1.0, -1.0);
	unsafe {
		assert_eq!(
			(s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 1, &mut k, &mut v),
			0
		);
	}
	assert_eq!((k, v), (0.5, 0.9));
}

/// Error codes: bad handle / non-parametric / out-of-range curveIndex /
/// out-of-range nth / null out.
#[test]
fn error_codes() {
	let s = ps();
	let mut out = 0.0;
	// 空句柄 → BadHandle。
	unsafe {
		assert_eq!(
			(s.parametric_param_get_value)(std::ptr::null_mut(), 0, 0.0, 0.5, &mut out),
			status::ERR_BAD_HANDLE
		);
	}
	// 空 out → ErrValue。
	let (_inst, _ih, ph) = make_instance();
	unsafe {
		assert_eq!(
			(s.parametric_param_get_value)(ph, 0, 0.0, 0.5, std::ptr::null_mut()),
			status::ERR_VALUE
		);
	}
	// curveIndex 越界（维度 1）→ BadIndex。
	unsafe {
		assert_eq!(
			(s.parametric_param_get_value)(ph, 1, 0.0, 0.5, &mut out),
			status::ERR_BAD_INDEX
		);
	}
	// nth 越界（2 点曲线）→ BadIndex。
	let mut k = 0.0;
	unsafe {
		assert_eq!(
			(s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 5, &mut k, &mut out),
			status::ERR_BAD_INDEX
		);
	}
	// 非 parametric 参数 → BadHandle（Integer 参数的句柄）。
	let desc = EffectDescriptor::new();
	let psuite = crate::suites::param::suite_v1();
	let dhandle = descriptor_handle(&desc);
	let t = cs("OfxParamTypeInteger");
	let n = cs("gain");
	let mut gph: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(psuite.param_define)(dhandle, t.as_ptr(), n.as_ptr(), &mut gph),
			0
		);
		// parametric 也定义一个，便于实例化后拿两个 handle。
		let pt = cs(crate::param::TYPE_PARAMETRIC);
		let pn = cs("curve");
		let mut pph: *mut c_void = std::ptr::null_mut();
		assert_eq!(
			(psuite.param_define)(dhandle, pt.as_ptr(), pn.as_ptr(), &mut pph),
			0
		);
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
		let ih = instance_handle(&inst);
		let mut g: *mut c_void = std::ptr::null_mut();
		let gn = cs("gain");
		assert_eq!(
			(psuite.param_get_handle)(ih, gn.as_ptr(), &mut g, std::ptr::null_mut()),
			0
		);
		assert_eq!(
			(s.parametric_param_get_value)(g, 0, 0.0, 0.5, &mut out),
			status::ERR_BAD_HANDLE
		);
		assert_eq!(
			(s.parametric_param_add_control_point)(g, 0, 0.0, 0.5, 0.5, 0),
			status::ERR_BAD_HANDLE
		);
	}
}

/// Driven by the dimension property: with dimension=2 the second
/// dimension works and the third is BadIndex; unconfigured curves read as
/// the identity default.
#[test]
fn dimension_prop_drives_curves() {
	let (desc, ph) = define_parametric();
	// 插件经属性 suite 把维度改成 2。
	let psuite = crate::suites::property::suite_v1();
	let dim = cs(crate::param::P_PARAMETRIC_DIMENSION);
	unsafe {
		assert_eq!((psuite.set_int)(ph, dim.as_ptr(), 0, 2), 0);
	}
	let s = ps();
	// 第二维：Add 后求值可用。
	unsafe {
		assert_eq!(
			(s.parametric_param_add_control_point)(ph, 1, 0.0, 0.25, 0.5, 0),
			0
		);
	}
	let mut out = 0.0;
	unsafe {
		assert_eq!(
			(s.parametric_param_get_value)(ph, 1, 0.0, 0.25, &mut out),
			0
		);
	}
	assert_eq!(out, 0.5);
	// 第一维未配置 → 恒等默认。
	unsafe {
		assert_eq!(
			(s.parametric_param_get_value)(ph, 0, 0.0, 0.25, &mut out),
			0
		);
	}
	assert_eq!(out, 0.25);
	let mut n = 0;
	unsafe {
		assert_eq!(
			(s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n),
			0
		);
	}
	assert_eq!(n, 2);
	// 第三维 → BadIndex。
	unsafe {
		assert_eq!(
			(s.parametric_param_get_value)(ph, 2, 0.0, 0.5, &mut out),
			status::ERR_BAD_INDEX
		);
		assert_eq!(
			(s.parametric_param_add_control_point)(ph, 2, 0.0, 0.5, 0.5, 0),
			status::ERR_BAD_INDEX
		);
	}
	// 维度属性随 from_def 复制到实例（实例句柄上同样生效）。
	let (_inst, _ih, iph) = instantiate(desc);
	let mut n = -1;
	unsafe {
		assert_eq!(
			(s.parametric_param_get_n_control_points)(iph, 1, 0.0, &mut n),
			0
		);
	}
	assert_eq!(n, 3); // (0,0) (0.25,0.5) (1,1)
}

/// The range property feeds the identity default of unconfigured curves
/// (endpoints = range values).
#[test]
fn range_prop_shapes_implicit_identity() {
	let (_desc, ph) = define_parametric();
	let psuite = crate::suites::property::suite_v1();
	let dim = cs(crate::param::P_PARAMETRIC_DIMENSION);
	let range = cs(crate::param::P_PARAMETRIC_RANGE);
	unsafe {
		assert_eq!((psuite.set_int)(ph, dim.as_ptr(), 0, 2), 0);
		// range = (0, 255)。
		assert_eq!((psuite.set_double)(ph, range.as_ptr(), 0, 0.0), 0);
		assert_eq!((psuite.set_double)(ph, range.as_ptr(), 1, 255.0), 0);
	}
	let s = ps();
	// 未配置的第二维：恒等默认端点 = range。
	let (mut k, mut v) = (-1.0, -1.0);
	unsafe {
		assert_eq!(
			(s.parametric_param_get_nth_control_point)(ph, 1, 0.0, 1, &mut k, &mut v),
			0
		);
	}
	assert_eq!((k, v), (255.0, 255.0));
}

/// UI properties (UIColour double×3N / InteractBackground pointer) go
/// through the generic property suite set/get and are copied to the
/// instance by from_def.
#[test]
fn ui_props_set_get_and_copy() {
	let (desc, ph) = define_parametric();
	let psuite = crate::suites::property::suite_v1();
	// UIColour：3 个 double（维度 1 → 1 个 RGB 三元组）。
	let colour = cs(crate::param::P_PARAMETRIC_UI_COLOUR);
	let rgb: [f64; 3] = [1.0, 0.0, 0.5];
	unsafe {
		assert_eq!(
			(psuite.set_double_n)(ph, colour.as_ptr(), 3, rgb.as_ptr()),
			0
		);
	}
	// InteractBackground：任意指针。
	let bg = cs(crate::param::P_PARAMETRIC_INTERACT_BG);
	let sentinel = 0x1234usize as *mut c_void;
	unsafe {
		assert_eq!((psuite.set_pointer)(ph, bg.as_ptr(), 0, sentinel), 0);
	}
	// 读回。
	let mut got = [0.0; 3];
	unsafe {
		assert_eq!(
			(psuite.get_double_n)(ph, colour.as_ptr(), 3, got.as_mut_ptr()),
			0
		);
	}
	assert_eq!(got, rgb);
	let mut gotp: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!((psuite.get_pointer)(ph, bg.as_ptr(), 0, &mut gotp), 0);
	}
	assert_eq!(gotp, sentinel);
	// 实例 props 深拷贝。
	let (_inst, _ih, iph) = instantiate(desc);
	let mut got2 = [0.0; 3];
	unsafe {
		assert_eq!(
			(psuite.get_double_n)(iph, colour.as_ptr(), 3, got2.as_mut_ptr()),
			0
		);
	}
	assert_eq!(got2, rgb);
	let mut gotp2: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!((psuite.get_pointer)(iph, bg.as_ptr(), 0, &mut gotp2), 0);
	}
	assert_eq!(gotp2, sentinel);
}

/// Instance edits go through the instance-changed notify path (with no
/// node bound it is a no-op; passing means it does not panic; the
/// throttling semantics of the registration path are covered by the param
/// suite tests).
#[test]
fn instance_edits_go_through_notify_path() {
	let (_inst, _ih, ph) = make_instance();
	let s = ps();
	unsafe {
		assert_eq!(
			(s.parametric_param_add_control_point)(ph, 0, 0.0, 0.2, 0.8, 0),
			0
		);
		assert_eq!(
			(s.parametric_param_set_nth_control_point)(ph, 0, 0.0, 1, 0.2, 0.9, 0),
			0
		);
		assert_eq!((s.parametric_param_delete_control_point)(ph, 0, 1), 0);
		assert_eq!((s.parametric_param_delete_all_control_points)(ph, 0), 0);
	}
	let mut out = 0.0;
	unsafe {
		assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.2, &mut out), 0);
	}
	assert_eq!(out, 0.2);
}
