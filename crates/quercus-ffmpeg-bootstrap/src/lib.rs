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

//! Locates a static FFmpeg development prefix. Windows downloads the
//! checksum-pinned BtbN archive; Linux and macOS build FFmpeg from source.

use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Prebuilt {
	asset: &'static str,
	sha256: &'static str,
}

pub fn ensure_prefix() -> PathBuf {
	if let Some(prefix) = env_or_dotenv("FFMPEG_DIR").map(PathBuf::from).filter(|dir| valid_prefix(dir)) {
		return prefix;
	}
	if std::env::var_os("QUERCUS_FFMPEG_OFFLINE").is_some() {
		panic!("FFMPEG_DIR is missing or invalid and QUERCUS_FFMPEG_OFFLINE is set");
	}
	let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set"));
	let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
	let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
	let prebuilt = match (target_os.as_str(), arch.as_str()) {
		("windows", "x86_64") => Prebuilt {
			asset: "ffmpeg-n8.1-latest-win64-gpl-8.1.zip",
			sha256: "15d6ab92a0bb22aceffc8c363150c37a4a40a90cabbaef325ab30b580ff24a71",
		},
		("linux", "x86_64" | "aarch64") | ("macos", _) => {
			return build_static_ffmpeg();
		}
		_ => panic!("no BtbN FFmpeg build for target {target_os}/{arch}"),
	};
	download_prebuilt(&out, &prebuilt)
}

fn build_static_ffmpeg() -> PathBuf {
	let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
	let root = manifest
		.ancestors()
		.find(|path| path.join("tooling/ffmpeg/build-ffmpeg.sh").is_file())
		.map(Path::to_path_buf)
		.or_else(|| {
			manifest.parent().and_then(|parent| {
				fs::read_dir(parent).ok()?.filter_map(Result::ok).find_map(|entry| {
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
		.expect("locate the FFmpeg bootstrap crate");
	let script = root.join("tooling/ffmpeg/build-ffmpeg.sh");
	assert!(script.is_file(), "packaged FFmpeg build script is missing: {}", script.display());
	let lock = root.join(".cache/build.lock");
	fs::create_dir_all(lock.parent().unwrap()).expect("create FFmpeg lock directory");
	let mut acquired = false;
	for _ in 0..3600 {
		match fs::create_dir(&lock) {
			Ok(()) => {
				acquired = true;
				break;
			}
			Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
				std::thread::sleep(std::time::Duration::from_secs(1));
			}
			Err(error) => panic!("create FFmpeg build lock: {error}"),
		}
	}
	assert!(acquired, "timed out waiting for the FFmpeg build lock");
	let status = Command::new("bash")
		.arg(&script)
		.current_dir(&root)
		.status()
		.expect("run the static FFmpeg build script");
	let _ = fs::remove_dir(&lock);
	assert!(status.success(), "static FFmpeg build failed");
	let prefix = root.join(".cache/ffmpeg");
	assert!(valid_prefix(&prefix), "static FFmpeg build has no lib/pkgconfig");
	prefix
}

fn valid_prefix(dir: &Path) -> bool {
	dir.join("lib").join("pkgconfig").is_dir()
}

fn download_prebuilt(out: &Path, prebuilt: &Prebuilt) -> PathBuf {
	let prefix = out.join("ffmpeg-prefix");
	let marker = out.join(format!(".ffmpeg-{}-complete", prebuilt.sha256));
	if valid_prefix(&prefix) && marker.exists() {
		return prefix;
	}
	let archive = out.join(prebuilt.asset);
	let url = format!(
		"https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/{}",
		prebuilt.asset
	);
	if !archive.exists() {
		println!("cargo:warning=downloading {url}");
		let status = Command::new("curl")
			.args(["-fL", "--retry", "3", "--output"])
			.arg(&archive)
			.arg(&url)
			.status()
			.expect("curl is required to download BtbN FFmpeg");
		assert!(status.success(), "failed to download {url}");
	}
	let actual = sha256(&archive);
	assert_eq!(actual, prebuilt.sha256, "SHA256 mismatch for {url}");
	let extract = out.join("ffmpeg-extract");
	if extract.exists() {
		fs::remove_dir_all(&extract).expect("clear old FFmpeg extraction");
	}
	fs::create_dir_all(&extract).expect("create FFmpeg extraction directory");
	if prebuilt.asset.ends_with(".zip") {
		let status = Command::new("powershell")
			.args(["-NoProfile", "-Command", "Expand-Archive"])
			.arg("-LiteralPath")
			.arg(&archive)
			.arg("-DestinationPath")
			.arg(&extract)
			.arg("-Force")
			.status()
			.expect("PowerShell is required to unpack BtbN FFmpeg");
		assert!(status.success(), "failed to unzip {}", archive.display());
	} else {
		let status = Command::new("tar")
			.arg("-xJf")
			.arg(&archive)
			.arg("-C")
			.arg(&extract)
			.status()
			.expect("tar is required to unpack BtbN FFmpeg");
		assert!(status.success(), "failed to extract {}", archive.display());
	}
	if prefix.exists() {
		fs::remove_dir_all(&prefix).expect("replace old FFmpeg prefix");
	}
	let inner = fs::read_dir(&extract)
		.expect("read FFmpeg extraction")
		.filter_map(Result::ok)
		.map(|entry| entry.path())
		.find(|path| path.is_dir())
		.expect("BtbN archive contains one prefix directory");
	fs::rename(inner, &prefix).expect("install FFmpeg prefix");
	assert!(valid_prefix(&prefix), "BtbN prefix has no lib/pkgconfig");
	fs::write(marker, prebuilt.sha256).expect("record verified FFmpeg prefix");
	prefix
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
