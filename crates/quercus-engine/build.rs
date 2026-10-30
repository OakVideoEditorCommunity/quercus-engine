//! Relocatable cdylib naming.
//!
//! The SDK and applications link `libquercus_engine` dynamically. Without an
//! install name/soname the linker records the absolute build path, which makes
//! installed binaries non-relocatable. Applications must add an rpath
//! (`@loader_path` / `$ORIGIN`) when they package the dylib next to the
//! executable.

fn main() {
	// Expose this crate's build-script OUT_DIR to the SDK's build script via
	// `links` metadata: some Cargo configurations place the cdylib there
	// instead of `<profile>/deps`.
	let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set");
	println!("cargo:engine_out={out_dir}");
	match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
		Ok("macos") => {
			println!(
				"cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libquercus_engine.dylib"
			);
			// The static FFmpeg closure references the system zlib as
			// @rpath/libz.1.dylib; the dylib itself must be able to find it.
			println!("cargo:rustc-cdylib-link-arg=-Wl,-rpath,/usr/lib");
		}
		Ok("linux") => {
			println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libquercus_engine.so");
		}
		_ => {}
	}
}
