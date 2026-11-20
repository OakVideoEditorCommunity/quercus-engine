use super::*;

/// The default path table includes every home- and app-relative location
/// plus the platform-gated OFX standard locations.
#[test]
fn default_paths_cover_home_and_standard_locations() {
	let home = Path::new("/home/octa");
	let paths = default_plugin_paths(Some(home));
	// Olive legacy + home locations.
	assert!(paths.contains(&home.join(".OFX/Plugins")));
	assert!(paths.contains(&home.join(".local/share/OFX/Plugins")));
	assert!(paths.contains(&home.join(".local/share/olive/ofx/Plugins")));
	// App-relative (C++ parity).
	assert!(paths.contains(&PathBuf::from("../OFX/Plugins")));
	assert!(paths.contains(&PathBuf::from("../share/olive/ofx/Plugins")));
	assert!(paths.contains(&PathBuf::from("../lib/olive/ofx/Plugins")));
	// OFX standard locations, gated by platform.
	#[cfg(target_os = "macos")]
	{
		assert!(paths.contains(&home.join("Library/OFX/Plugins")));
		assert!(paths.contains(&PathBuf::from("/Library/OFX/Plugins")));
	}
	#[cfg(target_os = "linux")]
	{
		assert!(paths.contains(&PathBuf::from("/usr/OFX/Plugins")));
		assert!(paths.contains(&PathBuf::from("/usr/local/OFX/Plugins")));
	}
	#[cfg(target_os = "windows")]
	{
		assert!(paths.iter().any(|p| p.ends_with("Common Files/OFX/Plugins")
			|| p.ends_with("Common Files\\OFX\\Plugins")));
	}
}

/// With no `$HOME` the home-based entries are skipped; the rest remain.
#[test]
fn default_paths_skip_home_entries_when_home_unset() {
	let paths = default_plugin_paths(None);
	assert!(!paths
		.iter()
		.any(|p| p.to_string_lossy().starts_with("/home/")));
	assert!(paths.contains(&PathBuf::from("../OFX/Plugins")));
	assert_eq!(
		paths
			.iter()
			.filter(|p| p.to_string_lossy().contains(".OFX"))
			.count(),
		0
	);
}

/// The dedupe record rejects an already-seen path and keeps first-seen
/// order.
#[test]
fn record_scanned_path_dedupes() {
	let mut seen: Vec<PathBuf> = Vec::new();
	assert!(record_scanned_path(&mut seen, PathBuf::from("/a/Plugins")));
	assert!(record_scanned_path(&mut seen, PathBuf::from("/b/Plugins")));
	// A duplicate is rejected without being recorded again.
	assert!(!record_scanned_path(&mut seen, PathBuf::from("/a/Plugins")));
	assert_eq!(
		seen,
		vec![PathBuf::from("/a/Plugins"), PathBuf::from("/b/Plugins")]
	);
}

/// Scanning an existing directory twice only records it once, and a
/// non-existent directory is skipped without error.
#[test]
fn scan_path_skips_missing_and_dedupes_existing() {
	let dir = std::env::temp_dir().join(format!("quercusplugin-scan-{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();
	let cache = PluginCache {
		plugins: Mutex::new(Vec::new()),
		scanned_paths: Mutex::new(Vec::new()),
		binaries: Mutex::new(Vec::new()),
	};
	cache.scan_path(&dir).expect("existing dir scans");
	cache.scan_path(&dir).expect("duplicate scan is a no-op");
	{
		let seen = cache
			.scanned_paths
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		assert_eq!(seen.len(), 1, "the same directory is scanned only once");
	} // drop the guard before scanning again

	// A missing directory is skipped (no error, no plugin scan).
	let missing = dir.join("does-not-exist");
	cache
		.scan_path(&missing)
		.expect("missing dir scan is a no-op");
	let plugins = cache.plugins.lock().unwrap_or_else(|e| e.into_inner());
	assert!(plugins.is_empty(), "no bundles were loaded");

	let _ = std::fs::remove_dir_all(&dir);
}

// ---- branch coverage batch: loader/cache/host ------------------------

fn temp_dir(tag: &str) -> PathBuf {
	let dir = std::env::temp_dir().join(format!(
		"quercus-ofx-plugin-host-{}-{tag}",
		std::process::id()
	));
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).unwrap();
	dir
}

fn test_cache() -> PluginCache {
	PluginCache {
		plugins: Mutex::new(Vec::new()),
		scanned_paths: Mutex::new(Vec::new()),
		binaries: Mutex::new(Vec::new()),
	}
}

/// The minimal test-plugin shared library built by build.rs.
fn fixture_lib() -> Option<PathBuf> {
	let out = PathBuf::from(env!("OUT_DIR"));
	let lib = out.join(if cfg!(target_os = "macos") {
		"quercus_test_plugin.dylib"
	} else {
		"quercus_test_plugin.so"
	});
	lib.is_file().then_some(lib)
}

/// Assembles a `.ofx.bundle` containing the test-plugin binary on the fly (platform
/// directory probing).
fn make_fixture_bundle(tag: &str) -> Option<(PathBuf, PathBuf)> {
	let lib = fixture_lib()?;
	let platform = if cfg!(target_os = "macos") {
		"MacOS"
	} else if cfg!(target_os = "windows") {
		"Win64"
	} else {
		"Linux-x86-64"
	};
	let root = temp_dir(tag);
	let bundle = root.join("quercus-test-plugin.ofx.bundle");
	let bin_dir = bundle.join("Contents").join(platform);
	std::fs::create_dir_all(&bin_dir).unwrap();
	let target = bin_dir.join(if cfg!(target_os = "windows") {
		"plugin.dll"
	} else {
		"plugin"
	});
	std::fs::copy(&lib, &target).unwrap();
	Some((root, bundle))
}

fn action_is(action: *const c_char, name: &str) -> bool {
	!action.is_null() && unsafe { CStr::from_ptr(action) }.to_bytes() == name.as_bytes()
}

unsafe extern "C" fn entry_ok(
	_: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	status::OK
}

unsafe extern "C" fn entry_reply_default(
	_: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	status::REPLY_DEFAULT
}

unsafe extern "C" fn entry_describe_fails(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	if action_is(action, ACTION_DESCRIBE_IN_CONTEXT) {
		status::ERR_FATAL
	} else {
		status::OK
	}
}

unsafe extern "C" fn entry_create_fails(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	if action_is(action, ACTION_CREATE_INSTANCE) {
		status::ERR_FATAL
	} else {
		status::OK
	}
}

fn fake_plugin(identifier: &str, contexts: Vec<String>, entry: EntryPoint) -> Arc<Plugin> {
	Arc::new(Plugin {
		identifier: identifier.into(),
		version: (1, 0),
		bundle_path: PathBuf::new(),
		contexts,
		descriptor: EffectDescriptor::new(),
		lib: std::ptr::null_mut(),
		entry,
		ofx_plugin: std::ptr::null_mut(),
		unloaded: std::sync::atomic::AtomicBool::new(false),
	})
}

/// An isolated Host (local cache/instances, avoiding the process singleton).
fn local_host(plugins: Vec<Arc<Plugin>>) -> Host {
	Host {
		props: PropertySet::new(),
		cache: PluginCache {
			plugins: Mutex::new(plugins),
			scanned_paths: Mutex::new(Vec::new()),
			binaries: Mutex::new(Vec::new()),
		},
		instances: Mutex::new(Vec::new()),
	}
}

// ---- dlopen/dlsym error paths ----------------------------------------

#[cfg(not(target_os = "windows"))]
#[test]
fn dl_open_and_dl_sym_error_paths_return_none() {
	// 不存在的库 → None。
	assert!(dl_open(Path::new("/definitely/not/a/library-quercus.so")).is_none());
	// 可加载的库 + 不存在的符号 → None。
	let lib = fixture_lib().expect("build.rs 产出测试插件库");
	let handle = dl_open(&lib).expect("测试插件库可加载");
	assert!(dl_sym(handle, "quercus_definitely_missing_symbol").is_none());
	unsafe { dlclose(handle) };
}

// ---- fetchSuite 入口 -------------------------------------------------

#[test]
fn host_fetch_suite_null_args_and_trace_paths() {
	use std::ffi::c_void as cv;
	static TRACE_LOCK: Mutex<()> = Mutex::new(());
	let _g = TRACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

	let host_set = PropertySet::new();
	let host = &host_set as *const PropertySet as *mut cv;
	let name = CString::new("OfxPropertySuite").unwrap();
	let bogus = CString::new("OfxBogusSuite").unwrap();

	// NULL host / NULL name → None（不触达 suite 分发表）。
	assert!(unsafe { host_fetch_suite(std::ptr::null_mut(), name.as_ptr(), 1) }.is_null());
	assert!(unsafe { host_fetch_suite(host, std::ptr::null(), 1) }.is_null());

	// QUERCUS_OFX_TRACE 打开：hit 与 miss 诊断分支。
	let prev = std::env::var_os("QUERCUS_OFX_TRACE");
	std::env::set_var("QUERCUS_OFX_TRACE", "1");
	assert!(!unsafe { host_fetch_suite(host, name.as_ptr(), 1) }.is_null());
	assert!(unsafe { host_fetch_suite(host, bogus.as_ptr(), 1) }.is_null());
	// 版本不符也走 miss。
	assert!(unsafe { host_fetch_suite(host, name.as_ptr(), 2) }.is_null());
	match prev {
		Some(v) => std::env::set_var("QUERCUS_OFX_TRACE", v),
		None => std::env::remove_var("QUERCUS_OFX_TRACE"),
	}
}

// ---- overlay interact 入口声明 ---------------------------------------

#[test]
fn overlay_interact_entry_null_pointer_and_v1_fallback() {
	unsafe extern "C" fn overlay(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		status::OK
	}
	let props = PropertySet::new();
	// 属性缺失 → None。
	assert!(overlay_interact_entry(&props).is_none());
	// 空指针 → 跳过（V2 空、V1 也缺）。
	props.set_one(
		PROP_OVERLAY_INTERACT_V2,
		Value::Pointer(std::ptr::null_mut()),
	);
	assert!(overlay_interact_entry(&props).is_none());
	// 类型不是 Pointer → 跳过。
	props.set_one(PROP_OVERLAY_INTERACT_V2, Value::Int(1));
	assert!(overlay_interact_entry(&props).is_none());
	// V1 回退命中。
	props.set_one(
		PROP_OVERLAY_INTERACT_V1,
		Value::Pointer(overlay as *const () as *mut std::ffi::c_void),
	);
	assert!(overlay_interact_entry(&props).is_some());
	// V2 优先。
	props.set_one(
		PROP_OVERLAY_INTERACT_V2,
		Value::Pointer(overlay as *const () as *mut std::ffi::c_void),
	);
	assert!(overlay_interact_entry(&props).is_some());
}

// ---- 卸载后的入口保护 -------------------------------------------------

#[test]
fn unloaded_plugin_actions_fail_without_touching_entry() {
	let plugin = fake_plugin("test.unloaded", vec![], entry_ok);
	plugin
		.unloaded
		.store(true, std::sync::atomic::Ordering::Release);
	let empty = PropertySet::new();
	let st = unsafe { plugin.call_action(ACTION_LOAD, std::ptr::null_mut(), &empty, &empty) };
	assert_eq!(st, status::ERR_FATAL);
	let st =
		unsafe { plugin.call_entry(entry_ok, ACTION_LOAD, std::ptr::null_mut(), &empty, &empty) };
	assert_eq!(st, status::ERR_FATAL);
}

// ---- bundle/二进制探测 -----------------------------------------------

#[test]
fn is_bundle_dir_matches_suffixes_only() {
	assert!(is_bundle_dir(Path::new("/tmp/Foo.bundle")));
	assert!(is_bundle_dir(Path::new("Bar.plugin")));
	assert!(!is_bundle_dir(Path::new("/tmp/NotIt")));
	// 无 file_name（根路径）→ false。
	assert!(!is_bundle_dir(Path::new("/")));
}

#[test]
fn find_binary_in_bundle_prefers_platform_dir_and_falls_back_to_root() {
	let dir = temp_dir("find-bin");

	// 空 bundle → None。
	assert!(find_binary_in_bundle(&dir).is_none());

	// 根目录 .so 回退。
	std::fs::write(dir.join("libquercus.so"), b"not a real lib").unwrap();
	assert_eq!(
		find_binary_in_bundle(&dir).unwrap(),
		dir.join("libquercus.so")
	);

	// 平台目录优先，且跳过 .plist。
	let platform = dir.join("Contents").join("Linux-x86-64");
	std::fs::create_dir_all(&platform).unwrap();
	std::fs::write(platform.join("Info.plist"), b"meta").unwrap();
	std::fs::write(platform.join("plugin"), b"bin").unwrap();
	assert_eq!(
		find_binary_in_bundle(&dir).unwrap(),
		platform.join("plugin")
	);

	// 平台目录里只有 .plist → 回退根目录。
	std::fs::remove_file(platform.join("plugin")).unwrap();
	assert_eq!(
		find_binary_in_bundle(&dir).unwrap(),
		dir.join("libquercus.so")
	);

	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn walk_dirs_respects_depth_and_read_errors() {
	let root = temp_dir("walk");
	std::fs::write(root.join("file.txt"), b"x").unwrap();
	let a = root.join("a");
	let bundle = a.join("One.bundle");
	let b = a.join("b");
	let c = b.join("c");
	std::fs::create_dir_all(&bundle).unwrap();
	std::fs::create_dir_all(&c).unwrap();
	std::fs::create_dir_all(b.join("Two.bundle")).unwrap();

	let mut out = Vec::new();
	walk_dirs(&root, 0, 1, &mut out);
	assert_eq!(
		out,
		vec![bundle.clone()],
		"深度 1 内只发现 One.bundle（文件与更深目录跳过）"
	);

	// 深度上限内全收。
	let mut deep = Vec::new();
	walk_dirs(&root, 0, 4, &mut deep);
	assert_eq!(deep.len(), 2);

	// read_dir 失败（路径是文件）→ 空返回。
	let mut errs = Vec::new();
	walk_dirs(&root.join("file.txt"), 0, 4, &mut errs);
	assert!(errs.is_empty());

	let _ = std::fs::remove_dir_all(&root);
}

// ---- load_bundle ------------------------------------------------------

#[test]
fn load_bundle_reports_missing_and_invalid_binaries() {
	let cache = test_cache();

	// 空目录（无二进制）→ 记录并返回。
	let empty = temp_dir("load-empty");
	cache.load_bundle(&empty);
	assert_eq!(cache.count(), 0);

	// 根目录垃圾 .so → dlopen 失败，记录并返回。
	let bad = temp_dir("load-bad");
	std::fs::write(bad.join("garbage.so"), b"definitely not an ELF").unwrap();
	cache.load_bundle(&bad);
	assert_eq!(cache.count(), 0);
	assert!(cache
		.binaries
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.is_empty());

	let _ = std::fs::remove_dir_all(&empty);
	let _ = std::fs::remove_dir_all(&bad);
}

#[test]
fn load_bundle_loads_fixture_plugins_and_dedupes() {
	let Some((root, bundle)) = make_fixture_bundle("load-ok") else {
		eprintln!("SKIP: 测试插件未构建");
		return;
	};
	let cache = test_cache();
	cache.load_bundle(&bundle);
	assert_eq!(cache.count(), 6, "测试插件导出 6 个效果");
	assert!(cache.find("org.quercus.test-plugin").is_some());
	assert_eq!(
		cache
			.binaries
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.len(),
		1
	);

	// 同一 bundle 二次加载 → 路径去重（直接 dlclose 返回）。
	cache.load_bundle(&bundle);
	assert_eq!(cache.count(), 6);
	assert_eq!(
		cache
			.binaries
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.len(),
		1
	);

	// at()/find() 边界。
	assert!(cache.at(0).is_some());
	assert!(cache.at(usize::MAX).is_none());
	assert!(cache.find("org.quercus.test-plugin.nope").is_none());

	let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn load_bundle_ignores_duplicate_plugin_identifiers() {
	let Some((root1, bundle1)) = make_fixture_bundle("dup-a") else {
		eprintln!("SKIP: 测试插件未构建");
		return;
	};
	let Some((root2, bundle2)) = make_fixture_bundle("dup-b") else {
		eprintln!("SKIP: 测试插件未构建");
		return;
	};
	let cache = test_cache();
	cache.load_bundle(&bundle1);
	assert_eq!(cache.count(), 6);
	// 第二个 bundle 的 identifier 全部重复 → 忽略，但二进制登记两份。
	cache.load_bundle(&bundle2);
	assert_eq!(cache.count(), 6);
	assert_eq!(
		cache
			.binaries
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.len(),
		2
	);
	let _ = std::fs::remove_dir_all(&root1);
	let _ = std::fs::remove_dir_all(&root2);
}

/// A loadable library with no OFX export symbols → collect_plugins is empty (Linux
/// libc).
#[cfg(target_os = "linux")]
#[test]
fn collect_plugins_without_ofx_symbols_is_empty() {
	let Some(libc) = [
		"/lib/x86_64-linux-gnu/libc.so.6",
		"/usr/lib/x86_64-linux-gnu/libc.so.6",
		"/lib64/libc.so.6",
		"/usr/lib64/libc.so.6",
		"/lib/libc.so.6",
		"/usr/lib/libc.so.6",
	]
	.iter()
	.map(PathBuf::from)
	.find(|p| p.is_file()) else {
		eprintln!("SKIP: 未找到 libc");
		return;
	};
	let Some(handle) = dl_open(&libc) else {
		eprintln!("SKIP: libc 不可 dlopen");
		return;
	};
	let cache = test_cache();
	let plugins = unsafe { cache.collect_plugins(handle, Path::new("no-ofx")) };
	unsafe { dlclose(handle) };
	assert!(plugins.is_empty());
}

/// A loadable bundle with no plugins → logs no usable plugins and dlcloses.
#[cfg(target_os = "linux")]
#[test]
fn load_bundle_without_ofx_plugins_reports_and_unloads() {
	let Some(libc) = [
		"/lib/x86_64-linux-gnu/libc.so.6",
		"/usr/lib/x86_64-linux-gnu/libc.so.6",
		"/lib64/libc.so.6",
		"/usr/lib64/libc.so.6",
		"/lib/libc.so.6",
		"/usr/lib/libc.so.6",
	]
	.iter()
	.map(PathBuf::from)
	.find(|p| p.is_file()) else {
		eprintln!("SKIP: 未找到 libc");
		return;
	};
	let dir = temp_dir("load-no-plugin");
	let platform = dir.join("Contents").join("Linux-x86-64");
	std::fs::create_dir_all(&platform).unwrap();
	std::os::unix::fs::symlink(&libc, platform.join("plugin")).unwrap();

	let cache = test_cache();
	cache.load_bundle(&dir);
	assert_eq!(cache.count(), 0);
	assert!(cache
		.binaries
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.is_empty());

	let _ = std::fs::remove_dir_all(&dir);
}

// ---- build_plugin 拒绝与上下文过滤 -----------------------------------

unsafe extern "C" fn noop_set_host(_: *mut OfxHost) {}

fn raw_ofx_plugin(
	api: Option<&CString>,
	api_version: c_int,
	identifier: Option<&CString>,
	set_host: Option<unsafe extern "C" fn(*mut OfxHost)>,
	main_entry: Option<EntryPoint>,
) -> OfxPlugin {
	OfxPlugin {
		plugin_api: api.map_or(std::ptr::null(), |c| c.as_ptr()),
		api_version,
		plugin_identifier: identifier.map_or(std::ptr::null(), |c| c.as_ptr()),
		plugin_version_major: 1,
		plugin_version_minor: 0,
		set_host,
		main_entry,
	}
}

unsafe extern "C" fn entry_multi_contexts(
	_: *const c_char,
	handle: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	if !handle.is_null() {
		let props = unsafe { &*crate::suites::tag::strip(handle as *mut c_void) };
		props.define(
			PROP_SUPPORTED_CONTEXTS,
			vec![
				Value::String(CString::new("OfxImageEffectContextGeneral").unwrap()),
				Value::String(CString::new("OfxImageEffectContextTransition").unwrap()),
				Value::String(CString::new("OfxImageEffectContextGenerator").unwrap()),
				Value::String(CString::new("OfxImageEffectContextFilter").unwrap()),
				Value::String(CString::new("OfxBogusContext").unwrap()),
			],
		);
	}
	status::OK
}

unsafe extern "C" fn entry_bogus_contexts(
	_: *const c_char,
	handle: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	if !handle.is_null() {
		let props = unsafe { &*crate::suites::tag::strip(handle as *mut c_void) };
		props.define(
			PROP_SUPPORTED_CONTEXTS,
			vec![Value::String(
				CString::new("OfxImageEffectContextRetimer").unwrap(),
			)],
		);
	}
	status::OK
}

unsafe extern "C" fn entry_load_fails(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	if action_is(action, ACTION_LOAD) {
		status::ERR_FATAL
	} else {
		status::OK
	}
}

unsafe extern "C" fn entry_build_describe_fails(
	action: *const c_char,
	_: *const c_void,
	_: *mut c_void,
	_: *mut c_void,
) -> c_int {
	if action_is(action, ACTION_DESCRIBE) {
		status::ERR_FATAL
	} else {
		status::OK
	}
}

#[test]
fn build_plugin_rejects_invalid_headers_and_filters_contexts() {
	let cache = test_cache();
	let bundle = Path::new("test-build.ofx.bundle");
	let api = CString::new("OfxImageEffectPluginAPI").unwrap();
	let wrong_api = CString::new("OfxSomethingElsePluginAPI").unwrap();
	let identifier = CString::new("test.build").unwrap();

	// NULL plugin_api → None。
	let mut ofx = raw_ofx_plugin(None, 1, Some(&identifier), None, Some(entry_ok));
	assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

	// API 名不符 → None。
	let mut ofx = raw_ofx_plugin(Some(&wrong_api), 1, Some(&identifier), None, Some(entry_ok));
	assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

	// API 版本不符 → None。
	let mut ofx = raw_ofx_plugin(Some(&api), 2, Some(&identifier), None, Some(entry_ok));
	assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

	// main_entry 缺 → None。
	let mut ofx = raw_ofx_plugin(Some(&api), 1, Some(&identifier), None, None);
	assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

	// load action 失败 → None（setHost 存在分支也被走到）。
	let mut ofx = raw_ofx_plugin(
		Some(&api),
		1,
		Some(&identifier),
		Some(noop_set_host),
		Some(entry_load_fails),
	);
	assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

	// describe action 失败 → None。
	let mut ofx = raw_ofx_plugin(
		Some(&api),
		1,
		Some(&identifier),
		None,
		Some(entry_build_describe_fails),
	);
	assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

	// describe 后无非标准上下文 → None。
	let mut ofx = raw_ofx_plugin(
		Some(&api),
		1,
		Some(&identifier),
		None,
		Some(entry_bogus_contexts),
	);
	assert!(unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }.is_none());

	// 四个标准上下文全收；非标准值被过滤。
	let mut ofx = raw_ofx_plugin(
		Some(&api),
		1,
		Some(&identifier),
		Some(noop_set_host),
		Some(entry_multi_contexts),
	);
	let plugin = unsafe { cache.build_plugin(&mut ofx, std::ptr::null_mut(), bundle) }
		.expect("标准上下文应接受");
	assert_eq!(plugin.identifier, "test.build");
	assert_eq!(plugin.contexts.len(), 4);
	assert!(!plugin.contexts.iter().any(|c| c == "OfxBogusContext"));
	assert_eq!(plugin.version, (1, 0));
}

// ---- scan 环境变量 ----------------------------------------------------

#[test]
fn scan_reads_plugin_path_env_vars() {
	static ENV_LOCK: Mutex<()> = Mutex::new(());
	let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some((root, _bundle)) = make_fixture_bundle("scan-env") else {
		eprintln!("SKIP: 测试插件未构建");
		return;
	};

	let prev = std::env::var_os("OLIVE_OFX_PLUGIN_PATH");
	std::env::set_var("OLIVE_OFX_PLUGIN_PATH", &root);
	let cache = test_cache();
	let result = cache.scan();
	match prev {
		Some(v) => std::env::set_var("OLIVE_OFX_PLUGIN_PATH", v),
		None => std::env::remove_var("OLIVE_OFX_PLUGIN_PATH"),
	}
	result.expect("scan");
	assert!(
		cache.find("org.quercus.test-plugin").is_some(),
		"环境变量目录里的 bundle 应被发现"
	);
	let _ = std::fs::remove_dir_all(&root);
}

// ---- create_instance / create_instance_preferred ---------------------

#[test]
fn create_instance_preferred_uses_custom_context_fallback() {
	let host = local_host(vec![fake_plugin(
		"test.pref.custom",
		vec!["OfxImageEffectContextCustom".into()],
		entry_ok,
	)]);
	let (context, inst) = host
		.create_instance_preferred("test.pref.custom")
		.expect("自定义上下文可实例化");
	assert_eq!(context, "OfxImageEffectContextCustom");
	assert_eq!(inst.value.context, "OfxImageEffectContextCustom");
	// 实例属性表（init_instance_props）已建好。
	assert!(inst.value.props.get("OfxPropType", 0).is_some());
	// 未登记标识 → NotFound。
	assert!(host.create_instance_preferred("nope").is_err());
}

#[test]
fn create_instance_preferred_returns_last_error() {
	let host = local_host(vec![fake_plugin(
		"test.pref.fail",
		vec![
			"OfxImageEffectContextFilter".into(),
			"OfxImageEffectContextGeneral".into(),
		],
		entry_describe_fails,
	)]);
	let err = host
		.create_instance_preferred("test.pref.fail")
		.err()
		.expect("全部上下文失败应返回错误");
	assert!(matches!(
		err,
		crate::error::Error::Failed(ref m) if m.contains("describeInContext")
	));
}

#[test]
fn create_instance_rejects_unsupported_context_and_missing_contexts() {
	let host = local_host(vec![fake_plugin(
		"test.ctx.one",
		vec!["OfxImageEffectContextFilter".into()],
		entry_ok,
	)]);
	// 插件不支持请求的上下文。
	assert!(host
		.create_instance("test.ctx.one", Some("OfxImageEffectContextGenerator"))
		.is_err());
	// None → 取首个支持上下文并成功。
	let inst = host
		.create_instance("test.ctx.one", None)
		.expect("首个上下文");
	assert_eq!(inst.value.context, "OfxImageEffectContextFilter");

	// 无支持上下文 → 失败。
	let none = local_host(vec![fake_plugin("test.ctx.none", vec![], entry_ok)]);
	assert!(none.create_instance("test.ctx.none", None).is_err());
	// 未登记标识 → 失败。
	assert!(none.create_instance("missing", None).is_err());

	// ReplyDefault 的 describeInContext/createInstance 也被接受。
	let default = local_host(vec![fake_plugin(
		"test.ctx.default",
		vec!["OfxImageEffectContextFilter".into()],
		entry_reply_default,
	)]);
	assert!(default.create_instance("test.ctx.default", None).is_ok());
}

#[test]
fn create_instance_action_failure_is_not_registered_alive() {
	let host = local_host(vec![fake_plugin(
		"test.create.fail",
		vec!["OfxImageEffectContextFilter".into()],
		entry_create_fails,
	)]);
	let err = host
		.create_instance("test.create.fail", None)
		.err()
		.expect("createInstance 失败");
	assert!(matches!(
		err,
		crate::error::Error::Failed(ref m) if m.contains("createInstance")
	));
	assert_eq!(host.alive_count(), 0);
}

#[test]
fn alive_count_prunes_released_instances() {
	let host = local_host(vec![fake_plugin(
		"test.alive",
		vec!["OfxImageEffectContextFilter".into()],
		entry_ok,
	)]);
	let a = host.create_instance("test.alive", None).expect("a");
	let b = host.create_instance("test.alive", None).expect("b");
	assert_eq!(host.alive_count(), 2);
	drop(a);
	assert_eq!(host.alive_count(), 1);
	drop(b);
	assert_eq!(host.alive_count(), 0);
}

/// `register_plugin_nodes` logs the reason and skips cache entries that cannot be
/// instantiated, and registers instantiable ones as dynamic nodes.
#[test]
fn register_plugin_nodes_skips_uninstantiable_and_registers_ok_plugins() {
	static LOCK: Mutex<()> = Mutex::new(());
	let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());

	let failing_id = "test.node-factory.fail";
	let ok_id = "test.node-factory.ok";
	{
		let mut plugins = Host::global()
			.cache
			.plugins
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		plugins.push(fake_plugin(failing_id, vec![], entry_ok));
		plugins.push(fake_plugin(
			ok_id,
			vec!["OfxImageEffectContextFilter".into()],
			entry_ok,
		));
	}

	let registered = crate::node_factory::register_plugin_nodes();

	{
		let mut plugins = Host::global()
			.cache
			.plugins
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		plugins.retain(|p| p.identifier != failing_id && p.identifier != ok_id);
	}

	assert!(
		registered.iter().any(|id| id == ok_id),
		"可实例化的插件应登记为动态节点"
	);
	assert!(
		!registered.iter().any(|id| id == failing_id),
		"无法实例化的插件应跳过"
	);
}
