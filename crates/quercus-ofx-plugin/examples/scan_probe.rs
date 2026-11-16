//! Throwaway probe: scan the real system OFX directory and print what the
//! host actually discovers (diagnosing why the effect library is empty).

use quercus_ofx_plugin::host;
use quercus_ofx_plugin::node_factory;
use quercus_ofx_plugin::property;
use quercus_ofx_plugin::suites;
use std::ffi;
use std::ptr;
fn main() {
	let host = host::Host::global();
	match host.cache.scan() {
		Ok(()) => println!("scan: ok"),
		Err(e) => println!("scan: FAILED: {e}"),
	}
	println!("plugins discovered: {}", host.cache.count());
	// Direct probe: read the host-level props through the C suite table.
	let suite = suites::property::suite_v1();
	unsafe {
		let handle = &host.props as *const property::PropertySet as *mut ffi::c_void;
		let name = ffi::CString::new("OfxPropName").unwrap();
		let mut out: *mut ffi::c_char = ptr::null_mut();
		let stat = (suite.get_string)(handle, name.as_ptr(), 0, &mut out);
		println!("direct propGetString(OfxPropName) -> {stat}");
		if stat == 0 && !out.is_null() {
			println!("  value: {:?}", ffi::CStr::from_ptr(out));
		}
	}
	let registered = node_factory::register_plugin_nodes();
	println!("factory node types registered: {}", registered.len());
	for id in &registered {
		println!("  type_id={id}");
	}
}
