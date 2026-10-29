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
	let profile_dir = Path::new(&out_dir)
		.ancestors()
		.nth(3)
		.expect("OUT_DIR is target/<profile>/build/<pkg>/out");
	println!(
		"cargo:rustc-link-search=native={}",
		profile_dir.join("deps").display()
	);
	println!("cargo:rustc-link-search=native={}", profile_dir.display());
	// The engine cdylib is built with an @rpath/$ORIGIN install name so it is
	// relocatable. The SDK's own test binaries live in the same `deps`
	// directory, so they can find it without an application rpath.
	if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
		println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
	} else if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
		println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
	}
}
