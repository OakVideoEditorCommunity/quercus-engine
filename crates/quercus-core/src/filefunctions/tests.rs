use super::*;
use std::sync::{Mutex, MutexGuard};

// The configuration-location tests mutate the process-wide `QUERCUS_CONFIG_DIR`
// env var, which is also read by `get_auto_recovery_root` and
// `config_location_path`. Rust runs tests in parallel, so serialize those
// env-dependent tests with a shared lock. We use the crate-wide test lock so
// `configstore` tests (which also mutate `QUERCUS_CONFIG_DIR`) serialize on the
// SAME mutex.
fn config_lock() -> &'static Mutex<()> {
	crate::test_support::env_lock()
}

fn unique_temp_dir(tag: &str) -> PathBuf {
	let dir = std::env::temp_dir().join(format!(
		"quercus-filefunctions-test-{}-{}-{}",
		tag,
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap()
			.as_nanos()
	));
	std::fs::create_dir_all(&dir).unwrap();
	dir
}

#[test]
fn fnv1a_known_vectors() {
	// Offset basis as lowercase hex, and the canonical FNV-1a-64 of "a".
	assert_eq!(fnv1a_hex(b""), "cbf29ce484222325");
	assert_eq!(fnv1a_hex(b"a"), "af63dc4c8601ec8c");
}

#[test]
fn ensure_extension_appends_and_is_case_insensitive() {
	let f = FileFunctions::new();
	assert_eq!(
		f.ensure_filename_extension("foo", "ove").unwrap(),
		"foo.ove"
	);
	// Already present (case-insensitive): untouched.
	assert_eq!(
		f.ensure_filename_extension("foo.OVE", "ove").unwrap(),
		"foo.OVE"
	);
	assert_eq!(
		f.ensure_filename_extension("foo.ove", "Ove").unwrap(),
		"foo.ove"
	);
	// The "." in the suffix must actually be present.
	assert_eq!(
		f.ensure_filename_extension("fooove", "ove").unwrap(),
		"fooove.ove"
	);
	// Empty inputs are no-ops.
	assert_eq!(f.ensure_filename_extension("", "ove").unwrap(), "");
	assert_eq!(f.ensure_filename_extension("foo", "").unwrap(), "foo");
}

#[test]
fn formatted_executable_for_platform() {
	let f = FileFunctions::new();
	let v = f.get_formatted_executable_for_platform("myapp").unwrap();
	#[cfg(target_os = "windows")]
	assert_eq!(v, "myapp.exe");
	#[cfg(not(target_os = "windows"))]
	assert_eq!(v, "myapp");
}

#[test]
fn unique_identifier_existing_and_missing() {
	let f = FileFunctions::new();
	let dir = unique_temp_dir("id");
	let file = dir.join("data.txt");
	std::fs::write(&file, b"hello").unwrap();

	let id = f
		.get_unique_file_identifier(&file.to_string_lossy())
		.unwrap();
	assert_eq!(id.len(), 16);
	assert!(id.chars().all(|c| c.is_ascii_hexdigit()));

	// Deterministic for the same file.
	let id2 = f
		.get_unique_file_identifier(&file.to_string_lossy())
		.unwrap();
	assert_eq!(id, id2);

	let missing = f
		.get_unique_file_identifier(&dir.join("nope.txt").to_string_lossy())
		.unwrap();
	assert_eq!(missing, "");
}

#[test]
fn read_file_as_string_roundtrip() {
	let f = FileFunctions::new();
	let dir = unique_temp_dir("read");
	let file = dir.join("x.txt");
	std::fs::write(&file, b"hello world").unwrap();
	assert_eq!(
		f.read_file_as_string(&file.to_string_lossy()).unwrap(),
		"hello world"
	);
	// Missing file -> empty string.
	assert_eq!(
		f.read_file_as_string(&dir.join("missing.txt").to_string_lossy())
			.unwrap(),
		""
	);
}

#[test]
fn directory_is_valid_create_semantics() {
	let f = FileFunctions::new();
	let parent = unique_temp_dir("dirval");
	let dir = parent.join("created");
	assert!(!dir.exists());
	assert!(f.directory_is_valid(&dir.to_string_lossy(), true));
	assert!(dir.is_dir());
	// Now it exists, so even without create it is valid.
	assert!(f.directory_is_valid(&dir.to_string_lossy(), false));

	let missing = parent.join("never");
	assert!(!missing.exists());
	assert!(!f.directory_is_valid(&missing.to_string_lossy(), false));
}

#[test]
fn safe_temporary_filename_skips_existing() {
	let f = FileFunctions::new();
	let dir = unique_temp_dir("safetmp");
	let original = dir.join("video.mp4");
	// Block the first candidate so the counter must advance.
	std::fs::write(dir.join("video.tmp0.mp4"), b"x").unwrap();

	let result = f
		.get_safe_temporary_filename(&original.to_string_lossy())
		.unwrap();
	assert!(result.ends_with(".mp4"), "suffix preserved: {}", result);
	assert!(
		result.contains(".tmp1.mp4"),
		"should skip existing .tmp0, got: {}",
		result
	);
	assert!(!Path::new(&result).exists());
}

#[test]
fn rename_allow_overwrite() {
	let f = FileFunctions::new();
	let dir = unique_temp_dir("rename");
	let from = dir.join("from.txt");
	let to = dir.join("to.txt");
	std::fs::write(&from, b"new").unwrap();
	std::fs::write(&to, b"old").unwrap();

	assert!(f.rename_file_allow_overwrite(&from.to_string_lossy(), &to.to_string_lossy()));
	assert!(!from.exists());
	assert_eq!(std::fs::read(&to).unwrap(), b"new");

	// Renaming a missing source fails.
	let missing = dir.join("missing.txt");
	assert!(!f.rename_file_allow_overwrite(
		&missing.to_string_lossy(),
		&dir.join("z.txt").to_string_lossy()
	));
}

#[test]
fn copy_directory_recursive_and_overwrite_check() {
	let f = FileFunctions::new();
	let src = unique_temp_dir("cp-src");
	let nested = src.join("sub");
	std::fs::create_dir_all(&nested).unwrap();
	std::fs::write(src.join("a.txt"), b"a").unwrap();
	std::fs::write(nested.join("b.txt"), b"b").unwrap();

	let dest = unique_temp_dir("cp-dst");
	// Nothing in dest yet: safe to copy without overwriting.
	assert!(
		f.can_copy_directory_without_overwriting(&src.to_string_lossy(), &dest.to_string_lossy())
	);
	f.copy_directory(&src.to_string_lossy(), &dest.to_string_lossy(), false)
		.unwrap();
	assert!(dest.join("a.txt").exists());
	assert!(dest.join("sub").join("b.txt").exists());
	assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"a");

	// Copying again would overwrite a.txt.
	assert!(
		!f.can_copy_directory_without_overwriting(&src.to_string_lossy(), &dest.to_string_lossy())
	);
	// With overwrite it succeeds and refreshes content.
	f.copy_directory(&src.to_string_lossy(), &dest.to_string_lossy(), true)
		.unwrap();
	assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"a");

	// A missing source is a silent no-op (returns Ok).
	f.copy_directory(
		&dest.join("nope").to_string_lossy(),
		&unique_temp_dir("cp-missing").to_string_lossy(),
		false,
	)
	.unwrap();
}

#[test]
fn application_and_temp_paths() {
	let f = FileFunctions::new();
	let app = f.get_application_path().unwrap();
	assert!(!app.is_empty());
	assert!(Path::new(&app).is_dir());

	let temp = f.get_temp_file_path().unwrap();
	assert!(!temp.is_empty());
	assert!(Path::new(&temp).is_dir());
}

#[test]
fn configuration_location_obeys_env_override() {
	let _guard: MutexGuard<()> = config_lock().lock().unwrap();
	let f = FileFunctions::new();
	let dir = unique_temp_dir("config");
	std::env::set_var("QUERCUS_CONFIG_DIR", &dir);
	let loc = f.get_configuration_location().unwrap();
	std::env::remove_var("QUERCUS_CONFIG_DIR");
	assert_eq!(PathBuf::from(&loc), dir);
	assert!(dir.is_dir());
}

#[test]
fn auto_recovery_root_derives_from_config() {
	let _guard: MutexGuard<()> = config_lock().lock().unwrap();
	let f = FileFunctions::new();
	let dir = unique_temp_dir("config2");
	std::env::set_var("QUERCUS_CONFIG_DIR", &dir);
	let root = f.get_auto_recovery_root().unwrap();
	std::env::remove_var("QUERCUS_CONFIG_DIR");
	assert!(root.ends_with("autorecovery"));
	assert!(PathBuf::from(&root).starts_with(&dir));
}

#[test]
fn config_location_path_helper() {
	let _guard: MutexGuard<()> = config_lock().lock().unwrap();
	let dir = unique_temp_dir("clp");
	std::env::set_var("QUERCUS_CONFIG_DIR", &dir);
	let p = config_location_path().unwrap();
	std::env::remove_var("QUERCUS_CONFIG_DIR");
	assert_eq!(p, dir);
}

#[test]
fn ends_with_case_insensitive_matrix() {
	assert!(ends_with_case_insensitive("foo.OVE", ".ove"));
	assert!(ends_with_case_insensitive("foo.ove", ".OVE"));
	assert!(ends_with_case_insensitive("x", "x"));
	assert!(ends_with_case_insensitive("x", ""));
	// Suffix longer than the string never matches.
	assert!(!ends_with_case_insensitive("ove", ".ove"));
	assert!(!ends_with_case_insensitive("", "a"));
	// Only ASCII A-Z fold (like the C++ range test); other bytes compare
	// exactly.
	assert!(ends_with_case_insensitive("a[", "["));
	assert!(!ends_with_case_insensitive("a[", "{"));
	assert!(ends_with_case_insensitive("vidéo.MP4", ".mp4"));
}

#[test]
fn ensure_extension_dotfile_multi_ext_and_unicode() {
	let f = FileFunctions::new();
	// Dotfiles and multi-extension names just get the suffix appended.
	assert_eq!(
		f.ensure_filename_extension(".hidden", "txt").unwrap(),
		".hidden.txt"
	);
	assert_eq!(
		f.ensure_filename_extension("archive.tar", "gz").unwrap(),
		"archive.tar.gz"
	);
	assert_eq!(
		f.ensure_filename_extension("archive.tar.gz", "gz").unwrap(),
		"archive.tar.gz"
	);
	// Unicode names pass through unchanged apart from the suffix.
	assert_eq!(
		f.ensure_filename_extension("動画ファイル", "ove").unwrap(),
		"動画ファイル.ove"
	);
	// A bare "." suffix (empty extension) is a no-op per the C++.
	assert_eq!(f.ensure_filename_extension("foo.", "").unwrap(), "foo.");
}

#[test]
fn safe_temporary_filename_naming_variants() {
	let f = FileFunctions::new();
	let dir = unique_temp_dir("safetmp2");

	// Counter 0 is used when nothing blocks it.
	let r0 = f
		.get_safe_temporary_filename(&dir.join("clip.mov").to_string_lossy())
		.unwrap();
	assert!(r0.ends_with("clip.tmp0.mov"), "got: {}", r0);

	// Complete suffix = everything from the FIRST dot (QFileInfo
	// completeSuffix semantics, `filefunctions.cpp:318-326`).
	let r1 = f
		.get_safe_temporary_filename(&dir.join("a.tar.gz").to_string_lossy())
		.unwrap();
	assert!(r1.ends_with("a.tmp0.tar.gz"), "got: {}", r1);

	// No extension at all.
	let r2 = f
		.get_safe_temporary_filename(&dir.join("README").to_string_lossy())
		.unwrap();
	assert!(r2.ends_with("README.tmp0"), "got: {}", r2);

	// Dotfile: basename is empty, complete suffix is the whole name.
	let r3 = f
		.get_safe_temporary_filename(&dir.join(".hidden").to_string_lossy())
		.unwrap();
	assert!(r3.ends_with(".tmp0.hidden"), "got: {}", r3);

	// No parent directory: the candidate is relative ("name.tmp0.ext").
	let r4 = f.get_safe_temporary_filename("video.mp4").unwrap();
	assert_eq!(r4, "video.tmp0.mp4");

	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unique_identifier_differs_by_path() {
	let f = FileFunctions::new();
	let dir = unique_temp_dir("id2");
	let a = dir.join("a.txt");
	let b = dir.join("b.txt");
	std::fs::write(&a, b"same").unwrap();
	std::fs::write(&b, b"same").unwrap();
	// The absolute path is part of the hash input
	// (`filefunctions.cpp:100-103`), so identical content in different
	// paths yields different identifiers.
	let ida = f.get_unique_file_identifier(&a.to_string_lossy()).unwrap();
	let idb = f.get_unique_file_identifier(&b.to_string_lossy()).unwrap();
	assert_ne!(ida, idb);

	// A directory also gets an identifier (C++ only checks exists()).
	let idd = f
		.get_unique_file_identifier(&dir.to_string_lossy())
		.unwrap();
	assert_eq!(idd.len(), 16);

	// Empty filename -> absolute() of "" is the CWD which exists; but a
	// definitely-bogus relative name yields "".
	let bogus = f
		.get_unique_file_identifier("definitely-not-here.xyz")
		.unwrap();
	assert_eq!(bogus, "");

	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_file_as_string_binary_and_lossy() {
	let f = FileFunctions::new();
	let dir = unique_temp_dir("read2");
	// Invalid UTF-8 is replaced (lossy), not an error.
	let bad = dir.join("bad.bin");
	std::fs::write(&bad, b"a\xff\xfeb").unwrap();
	let s = f.read_file_as_string(&bad.to_string_lossy()).unwrap();
	assert_eq!(s, "a\u{FFFD}\u{FFFD}b");

	// Empty file reads as empty, indistinguishable from a missing file.
	let empty = dir.join("empty.txt");
	std::fs::write(&empty, b"").unwrap();
	assert_eq!(f.read_file_as_string(&empty.to_string_lossy()).unwrap(), "");

	// A directory cannot be read as a file -> empty.
	assert_eq!(f.read_file_as_string(&dir.to_string_lossy()).unwrap(), "");

	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn directory_is_valid_blocked_by_file() {
	let f = FileFunctions::new();
	let parent = unique_temp_dir("dirval2");
	// A regular file blocks directory creation at the same path.
	let blocker = parent.join("blocked");
	std::fs::write(&blocker, b"x").unwrap();
	assert!(!f.directory_is_valid(&blocker.to_string_lossy(), true));
	// ...and so does a file in the MIDDLE of the path to create.
	let nested = blocker.join("child");
	assert!(!f.directory_is_valid(&nested.to_string_lossy(), true));
	// Nested creation works when nothing blocks it.
	let deep = parent.join("x").join("y").join("z");
	assert!(f.directory_is_valid(&deep.to_string_lossy(), true));
	assert!(deep.is_dir());

	let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn copy_directory_no_overwrite_preserves_existing() {
	let f = FileFunctions::new();
	let src = unique_temp_dir("cp2-src");
	std::fs::write(src.join("a.txt"), b"new").unwrap();
	let dest = unique_temp_dir("cp2-dst");
	std::fs::write(dest.join("a.txt"), b"old").unwrap();

	// would-overwrite detection fires on the direct conflict...
	assert!(
		!f.can_copy_directory_without_overwriting(&src.to_string_lossy(), &dest.to_string_lossy())
	);
	// ...and a missing source trivially reports "safe" (empty iteration,
	// `filefunctions.cpp:202`).
	assert!(f.can_copy_directory_without_overwriting(
		&src.join("nope").to_string_lossy(),
		&dest.to_string_lossy()
	));

	// overwrite=false: the copy of the conflicting file fails (logged)
	// and the destination keeps its old content; other files still copy.
	std::fs::write(src.join("b.txt"), b"b").unwrap();
	f.copy_directory(&src.to_string_lossy(), &dest.to_string_lossy(), false)
		.unwrap();
	assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"old");
	assert_eq!(std::fs::read(dest.join("b.txt")).unwrap(), b"b");

	// overwrite=true replaces the conflicting file.
	f.copy_directory(&src.to_string_lossy(), &dest.to_string_lossy(), true)
		.unwrap();
	assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"new");

	let _ = std::fs::remove_dir_all(&src);
	let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn can_copy_detects_nested_conflict() {
	let f = FileFunctions::new();
	let src = unique_temp_dir("cp3-src");
	std::fs::create_dir_all(src.join("sub")).unwrap();
	std::fs::write(src.join("sub").join("deep.txt"), b"x").unwrap();
	let dest = unique_temp_dir("cp3-dst");
	std::fs::create_dir_all(dest.join("sub")).unwrap();
	std::fs::write(dest.join("sub").join("deep.txt"), b"y").unwrap();
	// The conflict is two levels down: recursion must find it.
	assert!(
		!f.can_copy_directory_without_overwriting(&src.to_string_lossy(), &dest.to_string_lossy())
	);

	let _ = std::fs::remove_dir_all(&src);
	let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn rename_to_fresh_destination() {
	let f = FileFunctions::new();
	let dir = unique_temp_dir("rename2");
	let from = dir.join("only.txt");
	let to = dir.join("fresh.txt");
	std::fs::write(&from, b"data").unwrap();
	assert!(f.rename_file_allow_overwrite(&from.to_string_lossy(), &to.to_string_lossy()));
	assert_eq!(std::fs::read(&to).unwrap(), b"data");

	// Destination existing as a DIRECTORY cannot be removed by
	// remove_file, so the rename fails (documented CPP-PARITY
	// divergence from C++ fs::remove, which would remove an empty dir).
	let from2 = dir.join("only2.txt");
	std::fs::write(&from2, b"z").unwrap();
	let todir = dir.join("destdir");
	std::fs::create_dir(&todir).unwrap();
	assert!(!f.rename_file_allow_overwrite(&from2.to_string_lossy(), &todir.to_string_lossy()));

	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn temp_file_path_under_system_temp() {
	let f = FileFunctions::new();
	let temp = f.get_temp_file_path().unwrap();
	// `<temp>/quercus`, created on demand (`filefunctions.cpp:184-196`).
	assert_eq!(PathBuf::from(&temp), std::env::temp_dir().join("quercus"));
	assert!(Path::new(&temp).is_dir());
}

#[test]
fn unique_temp_dirs_are_actually_unique() {
	let a = unique_temp_dir("uniq");
	let b = unique_temp_dir("uniq");
	assert_ne!(a, b);
	let _ = std::fs::remove_dir_all(&a);
	let _ = std::fs::remove_dir_all(&b);
}
