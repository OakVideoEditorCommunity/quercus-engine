// `#[link(name = "quercus_engine")]` needs the linker to find the engine
// cdylib. Cargo builds it into the dependency dir but passes that dir only to
// rustc (`-L dependency=`), not to the system linker. Cargo also hashes
// dependency cdylibs (`libquercus_engine-<hash>.dylib`), which the plain
// `-lquercus_engine` cannot find, so stage a canonical copy in OUT_DIR.
use std::path::{Path, PathBuf};

fn main() {
	// docs.rs builds without the native engine (see Cargo.toml features).
	if std::env::var_os("DOCS_RS").is_some()
		|| std::env::var_os("CARGO_FEATURE_ENGINE").is_none()
	{
		return;
	}
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

	let mut artifact = None;
	if let Ok(engine_out) = std::env::var("DEP_QUERCUS_ENGINE_NATIVE_ENGINE_OUT") {
		println!("cargo:rustc-link-search=native={engine_out}");
		artifact = find_engine_artifact(Path::new(&engine_out));
	}
	if artifact.is_none() {
		artifact = find_engine_artifact(&deps)
			.or_else(|| find_engine_artifact(&profile_dir.join("build")));
	}

	let link_dir = match artifact {
		Some(path) => {
			let dir = stage_engine_artifact(&path, Path::new(&out_dir));
			// Also expose the dylib next to the final binaries and test
			// binaries, so an application rpath of @loader_path/$ORIGIN
			// works without knowing Cargo's hashed artifact path.
			// On Windows Cargo's own cdylib output is the unhashed
			// quercus_engine.dll in deps; copying there would collide with
			// the engine link step. The profile copy is enough for the exe,
			// and deps already gets the engine DLL from Cargo.
			#[cfg(not(target_os = "windows"))]
			publish_runtime_copy(&dir.join(canonical_name()), &profile_dir.join("deps"));
			publish_runtime_copy(&dir.join(canonical_name()), &profile_dir);
			dir
		}
		None => {
			// Let the linker report the missing library with a search path
			// that at least points at the dependency directory.
			deps.clone()
		}
	};

	println!("cargo:rustc-link-search=native={}", link_dir.display());
	println!("cargo:rustc-link-search=native={}", deps.display());
	println!("cargo:rustc-link-search=native={}", profile_dir.display());

	// The engine cdylib is built with an @rpath/$ORIGIN install name so it is
	// relocatable. The SDK's own test binaries live in the same `deps`
	// directory; the staged copy is the fallback.
	if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
		println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
		println!("cargo:rustc-link-arg=-Wl,-rpath,{}", link_dir.display());
	} else if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
		println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
		println!("cargo:rustc-link-arg=-Wl,-rpath,{}", link_dir.display());
	}
}

fn artifact_extension() -> &'static str {
	if cfg!(target_os = "macos") {
		"dylib"
	} else if cfg!(target_os = "windows") {
		"dll"
	} else {
		"so"
	}
}

fn canonical_name() -> String {
	if cfg!(target_os = "windows") {
		"quercus_engine.dll".to_string()
	} else {
		format!("libquercus_engine.{}", artifact_extension())
	}
}

fn is_engine_file(name: &str) -> bool {
	let lower = name.to_ascii_lowercase();
	(lower.starts_with("libquercus_engine") || lower.starts_with("quercus_engine"))
		&& (lower.ends_with(".dylib")
			|| lower.ends_with(".so")
			|| lower.ends_with(".dll")
			|| lower.ends_with(".lib"))
}

fn find_engine_artifact(root: &Path) -> Option<PathBuf> {
	let canonical = root.join(canonical_name());
	if canonical.is_file() {
		return Some(canonical);
	}
	let mut stack = vec![(root.to_path_buf(), 0usize)];
	while let Some((dir, depth)) = stack.pop() {
		if let Ok(entries) = std::fs::read_dir(&dir) {
			for entry in entries.flatten() {
				let path = entry.path();
				let Ok(kind) = entry.file_type() else {
					continue;
				};
				if kind.is_file() {
					if path
						.file_name()
						.and_then(|name| name.to_str())
						.is_some_and(is_engine_file)
					{
						return Some(path);
					}
				} else if kind.is_dir() && depth < 4 {
					stack.push((path, depth + 1));
				}
			}
		}
	}
	None
}

fn publish_runtime_copy(source: &Path, dir: &Path) {
	if std::fs::create_dir_all(dir).is_err() {
		return;
	}
	let target = dir.join(canonical_name());
	if source == target {
		return;
	}
	let _ = std::fs::copy(source, &target);
	#[cfg(unix)]
	if let Ok(meta) = std::fs::metadata(source) {
		use std::os::unix::fs::PermissionsExt;
		let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(meta.permissions().mode()));
	}
}

fn stage_engine_artifact(source: &Path, out_dir: &Path) -> PathBuf {
	let dir = out_dir.join("native");
	let target = dir.join(canonical_name());
	if std::fs::create_dir_all(&dir).is_err() {
		return source.parent().unwrap_or(out_dir).to_path_buf();
	}
	if std::fs::copy(source, &target).is_err() {
		return source.parent().unwrap_or(out_dir).to_path_buf();
	}
	#[cfg(unix)]
	if let Ok(meta) = std::fs::metadata(source) {
		use std::os::unix::fs::PermissionsExt;
		let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(meta.permissions().mode()));
	}
	// MSVC links against the import library, not the runtime DLL.
	if cfg!(target_os = "windows") {
		let import = source.with_extension("dll.lib");
		if import.is_file() {
			let _ = std::fs::copy(&import, dir.join("quercus_engine.dll.lib"));
		}
		let plain = source.with_extension("lib");
		if plain.is_file() {
			let _ = std::fs::copy(&plain, dir.join("quercus_engine.lib"));
		}
	}
	dir
}
