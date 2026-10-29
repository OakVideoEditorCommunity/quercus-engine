// `#[link(name = "quercus_engine")]` (src/audio.rs) needs the linker to find
// the engine cdylib. Cargo builds it into the dependency dir but passes
// that dir only to rustc (`-L dependency=`), not to the system linker —
// forward it as a native search path.
use std::path::Path;

fn main() {
	// docs.rs builds without the native engine (see Cargo.toml features).
	if std::env::var_os("DOCS_RS").is_some()
		|| std::env::var_os("CARGO_FEATURE_ENGINE").is_none()
	{
		return;
	}
	// OUT_DIR = target/<profile>/build/<pkg>-<hash>/out
	let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set for build scripts");
	// Build-script OUT_DIR layouts differ between plain cargo
	// (target/<profile>/build/<pkg>-<hash>/out) and instrumented builds
	// (target/llvm-cov-target/<profile>/build/<pkg>/<hash>/out): locate the
	// `build` component and use its parent instead of counting ancestors.
	let profile_dir = Path::new(&out_dir)
		.ancestors()
		.find(|path| path.file_name().is_some_and(|name| name == "build"))
		.and_then(Path::parent)
		.expect("OUT_DIR contains target/<profile>/build");
	let deps = profile_dir.join("deps");
	if let Ok(engine_out) = std::env::var("DEP_QUERCUS_ENGINE_NATIVE_ENGINE_OUT") {
		println!("cargo:rustc-link-search=native={engine_out}");
	}
	// Cargo normally places the cdylib in `<profile>/deps`, but some
	// configurations (notably instrumented `cargo test --tests` builds) put
	// it under `<profile>/build/quercus-engine/<hash>/out`. Locate it rather
	// than assuming one layout.
	let engine_dir = find_engine_library(&deps)
		.or_else(|| find_engine_library(&profile_dir.join("build")))
		.map(|path| path.parent().unwrap_or(profile_dir).to_path_buf())
		.unwrap_or_else(|| deps.clone());
	println!("cargo:rustc-link-search=native={}", engine_dir.display());
	println!("cargo:rustc-link-search=native={}", deps.display());
	println!("cargo:rustc-link-search=native={}", profile_dir.display());
	// The engine cdylib is built with an @rpath/$ORIGIN install name so it is
	// relocatable. The SDK's own test binaries live in the same `deps`
	// directory, so they can find it without an application rpath.
	if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
		println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
		println!("cargo:rustc-link-arg=-Wl,-rpath,{}", engine_dir.display());
	} else if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
		println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
		println!("cargo:rustc-link-arg=-Wl,-rpath,{}", engine_dir.display());
	}
}

fn find_engine_library(root: &Path) -> Option<std::path::PathBuf> {
	const NAMES: &[&str] = &[
		"libquercus_engine.dylib",
		"libquercus_engine.so",
		"quercus_engine.dll",
		"quercus_engine.lib",
	];
	let mut stack = vec![(root.to_path_buf(), 0usize)];
	while let Some((dir, depth)) = stack.pop() {
		for name in NAMES {
			let candidate = dir.join(name);
			if candidate.is_file() {
				return Some(candidate);
			}
		}
		if depth < 4 && let Ok(entries) = std::fs::read_dir(&dir) {
			for entry in entries.flatten() {
				if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
					stack.push((entry.path(), depth + 1));
				}
			}
		}
	}
	None
}
