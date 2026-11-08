//! **Internal implementation detail — do not depend on this crate directly.**
//!
//! It is published only for `quercus-engine`'s dependency graph. Its API is
//! not stable and may change without notice. Use `quercus-engine-rs`, or the
//! C ABI via `quercus-engine`, instead.

// Quercus Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Quercus Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Locates a static FFmpeg development prefix. Windows uses a pinned vcpkg
//! source archive; Linux and macOS use the project's FFmpeg build scripts.

use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const VCPKG_COMMIT: &str = "ec48c11eae9448c8c6e7739d93e06fa2433aa95f";
const VCPKG_SHA256: &str = "6fa8be98831a61a3f029f2bb1602b29c4e1fc1cf2dc4f3013049ffe0a8cfa47d";
const WINDOWS_TRIPLET: &str = "x64-windows-static-md";

pub fn ensure_prefix() -> PathBuf {
	println!("cargo:rerun-if-env-changed=QUERCUS_FFMPEG_CACHE");
	println!("cargo:rerun-if-env-changed=QUERCUS_FFMPEG_OFFLINE");
	if let Some(prefix) = env_or_dotenv("FFMPEG_DIR")
		.map(PathBuf::from)
		.filter(|dir| valid_prefix(dir))
	{
		return prefix;
	}
	if std::env::var_os("QUERCUS_FFMPEG_OFFLINE").is_some() {
		panic!("FFMPEG_DIR is missing or invalid and QUERCUS_FFMPEG_OFFLINE is set");
	}
	let target_os = target_os();
	let arch = std::env::var("CARGO_CFG_TARGET_ARCH")
		.unwrap_or_else(|_| std::env::consts::ARCH.to_string());
	match (target_os.as_str(), arch.as_str()) {
		("windows", "x86_64") => build_windows_ffmpeg(),
		("linux", "x86_64" | "aarch64") | ("macos", _) => build_static_ffmpeg(),
		_ => panic!("no static FFmpeg bootstrap for target {target_os}/{arch}"),
	}
}

fn build_static_ffmpeg() -> PathBuf {
	let root = bootstrap_root();
	let cache = build_cache();
	let prefix = cache.join("ffmpeg");
	let script = root.join("tooling/ffmpeg/build-ffmpeg.sh");
	assert!(
		script.is_file(),
		"packaged FFmpeg build script is missing: {}",
		script.display()
	);
	let _lock = BuildLock::acquire(cache.join("build.lock"));
	if valid_prefix(&prefix) {
		return prefix;
	}
	let status = Command::new("bash")
		.arg(&script)
		.current_dir(&root)
		.env("QUERCUS_FFMPEG_CACHE", &cache)
		.status()
		.expect("run the static FFmpeg build script");
	assert!(status.success(), "static FFmpeg build failed");
	assert!(
		valid_prefix(&prefix),
		"static FFmpeg build has no lib/pkgconfig"
	);
	prefix
}

fn build_windows_ffmpeg() -> PathBuf {
	assert_eq!(
		std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_else(|_| {
			if cfg!(target_env = "msvc") {
				"msvc".to_string()
			} else {
				String::new()
			}
		}),
		"msvc",
		"the pinned Windows FFmpeg build currently supports MSVC targets"
	);
	let cache = build_cache();
	let prefix = cache.join("vcpkg").join("installed").join(WINDOWS_TRIPLET);
	let marker = prefix.join(".quercus-ffmpeg-complete");
	if valid_prefix(&prefix) && marker.exists() {
		return prefix;
	}
	let _lock = BuildLock::acquire(cache.join("vcpkg-build.lock"));
	if valid_prefix(&prefix) && marker.exists() {
		return prefix;
	}
	fs::create_dir_all(&cache).expect("create vcpkg cache directory");
	let archive = cache.join(format!("vcpkg-{VCPKG_COMMIT}.zip"));
	let url = format!("https://github.com/microsoft/vcpkg/archive/{VCPKG_COMMIT}.zip");
	if !archive.exists() {
		println!("cargo:warning=downloading {url}");
		let status = Command::new("curl")
			.args(["-fL", "--retry", "3", "--output"])
			.arg(&archive)
			.arg(&url)
			.status()
			.expect("curl is required to download vcpkg");
		assert!(status.success(), "failed to download {url}");
	}
	let actual = sha256(&archive);
	assert_eq!(actual, VCPKG_SHA256, "SHA256 mismatch for {url}");
	let vcpkg = cache.join("vcpkg");
	let overlay = cache.join("triplets");
	fs::create_dir_all(&overlay).expect("create vcpkg triplet directory");
	fs::write(
		overlay.join(format!("{WINDOWS_TRIPLET}.cmake")),
		"set(VCPKG_TARGET_ARCHITECTURE x64)\n\
		 set(VCPKG_CRT_LINKAGE dynamic)\n\
		 set(VCPKG_LIBRARY_LINKAGE static)\n\
		 set(VCPKG_BUILD_TYPE release)\n",
	)
	.expect("write release-only vcpkg triplet");
	if !vcpkg.join("vcpkg.exe").is_file() {
		let extract = cache.join("vcpkg-extract");
		if extract.exists() {
			fs::remove_dir_all(&extract).expect("clear old vcpkg extraction");
		}
		fs::create_dir_all(&extract).expect("create vcpkg extraction directory");
		let status = Command::new("powershell")
			.args(["-NoProfile", "-Command", "Expand-Archive"])
			.arg("-LiteralPath")
			.arg(&archive)
			.arg("-DestinationPath")
			.arg(&extract)
			.arg("-Force")
			.status()
			.expect("PowerShell is required to unpack vcpkg");
		assert!(status.success(), "failed to unzip {}", archive.display());
		if vcpkg.exists() {
			fs::remove_dir_all(&vcpkg).expect("replace old vcpkg tree");
		}
		let inner = fs::read_dir(&extract)
			.expect("read vcpkg extraction")
			.filter_map(Result::ok)
			.map(|entry| entry.path())
			.find(|path| path.is_dir())
			.expect("vcpkg archive contains one source directory");
		fs::rename(inner, &vcpkg).expect("install vcpkg source tree");
		let status = Command::new("cmd")
			.args(["/C", "bootstrap-vcpkg.bat", "-disableMetrics"])
			.current_dir(&vcpkg)
			.status()
			.expect("bootstrap vcpkg");
		assert!(status.success(), "vcpkg bootstrap failed");
	}
	let status = Command::new(vcpkg.join("vcpkg.exe"))
		.arg("install")
		.arg("ffmpeg[ass,dav1d,freetype,fribidi,gpl,mp3lame,openjpeg,opus,snappy,speex,theora,vorbis,vpx,webp,x264,x265]:x64-windows-static-md")
		.args(["--triplet", WINDOWS_TRIPLET, "--clean-after-build", "--disable-metrics"])
		.arg(format!("--overlay-triplets={}", overlay.display()))
		.current_dir(&vcpkg)
		.status()
		.expect("build static FFmpeg with vcpkg");
	assert!(status.success(), "vcpkg static FFmpeg build failed");
	assert!(valid_prefix(&prefix), "vcpkg FFmpeg has no lib/pkgconfig");
	fs::write(marker, VCPKG_COMMIT).expect("record completed vcpkg FFmpeg prefix");
	prefix
}

fn bootstrap_root() -> PathBuf {
	let manifest =
		PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
	manifest
		.ancestors()
		.find(|path| path.join("tooling/ffmpeg/build-ffmpeg.sh").is_file())
		.map(Path::to_path_buf)
		.or_else(|| {
			manifest.parent().and_then(|parent| {
				fs::read_dir(parent)
					.ok()?
					.filter_map(Result::ok)
					.find_map(|entry| {
						let path = entry.path();
						let matches = path
							.file_name()
							.and_then(|name| name.to_str())
							.is_some_and(|name| name.starts_with("quercus-ffmpeg-bootstrap"))
							&& path.join("tooling/ffmpeg/build-ffmpeg.sh").is_file();
						matches.then_some(path)
					})
			})
		})
		.expect("locate the FFmpeg bootstrap crate")
}

fn build_cache() -> PathBuf {
	if let Some(path) = std::env::var_os("QUERCUS_FFMPEG_CACHE") {
		return PathBuf::from(path);
	}
	let Some(out) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
		return std::env::current_dir()
			.expect("current directory")
			.join("target/quercus-ffmpeg");
	};
	let profile = out
		.ancestors()
		.find(|path| path.file_name().is_some_and(|name| name == "build"))
		.and_then(Path::parent)
		.expect("OUT_DIR contains target/<profile>/build");
	profile.parent().unwrap_or(profile).join("quercus-ffmpeg")
}

fn target_os() -> String {
	std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_else(|_| std::env::consts::OS.to_string())
}

struct BuildLock(PathBuf);

impl BuildLock {
	fn acquire(path: PathBuf) -> Self {
		fs::create_dir_all(path.parent().unwrap()).expect("create build lock directory");
		for _ in 0..7200 {
			match fs::create_dir(&path) {
				Ok(()) => return Self(path),
				Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
					std::thread::sleep(std::time::Duration::from_secs(1));
				}
				Err(error) => panic!("create build lock: {error}"),
			}
		}
		panic!("timed out waiting for the FFmpeg build lock");
	}
}

impl Drop for BuildLock {
	fn drop(&mut self) {
		let _ = fs::remove_dir(&self.0);
	}
}

fn valid_prefix(dir: &Path) -> bool {
	let library = if target_os() == "windows" {
		dir.join("lib").join("avcodec.lib")
	} else {
		dir.join("lib").join("libavcodec.a")
	};
	dir.join("lib").join("pkgconfig").is_dir()
		&& dir
			.join("include")
			.join("libavutil")
			.join("avutil.h")
			.is_file()
		&& library.is_file()
}

fn sha256(path: &Path) -> String {
	let mut file = fs::File::open(path).expect("open downloaded FFmpeg archive");
	let mut hasher = Sha256::new();
	io::copy(&mut file, &mut hasher).expect("hash FFmpeg archive");
	format!("{:x}", hasher.finalize())
}

fn env_or_dotenv(key: &str) -> Option<String> {
	std::env::var(key).ok().or_else(|| {
		let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
		let dotenv = manifest.join("../..").join(".env");
		let text = fs::read_to_string(dotenv).ok()?;
		text.lines().find_map(|line| {
			let (name, value) = line.split_once('=')?;
			(name == key).then(|| value.trim().trim_matches('"').to_string())
		})
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn prefix_requires_completed_headers_and_library() {
		let root =
			std::env::temp_dir().join(format!("quercus-ffmpeg-prefix-test-{}", std::process::id()));
		fs::create_dir_all(root.join("lib/pkgconfig")).unwrap();
		assert!(!valid_prefix(&root));
		fs::create_dir_all(root.join("include/libavutil")).unwrap();
		fs::write(root.join("include/libavutil/avutil.h"), "").unwrap();
		assert!(!valid_prefix(&root));
		let library = if cfg!(target_os = "windows") {
			root.join("lib/avcodec.lib")
		} else {
			root.join("lib/libavcodec.a")
		};
		fs::write(library, "").unwrap();
		assert!(valid_prefix(&root));
		fs::remove_dir_all(root).unwrap();
	}
}
