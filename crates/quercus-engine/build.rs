//! Relocatable cdylib naming.
//!
//! The SDK and applications link `libquercus_engine` dynamically. Without an
//! install name/soname the linker records the absolute build path, which makes
//! installed binaries non-relocatable. Applications must add an rpath
//! (`@loader_path` / `$ORIGIN`) when they package the dylib next to the
//! executable.

fn main() {
	match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
		Ok("macos") => {
			println!(
				"cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libquercus_engine.dylib"
			);
		}
		Ok("linux") => {
			println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libquercus_engine.so");
		}
		_ => {}
	}
}
