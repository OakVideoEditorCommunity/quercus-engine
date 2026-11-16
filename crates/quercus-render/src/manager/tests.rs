use super::*;
use std::sync::{Mutex, MutexGuard};

/// Serializes the manager-singleton tests (the singleton is global).
static MANAGER_TEST_LOCK: Mutex<()> = Mutex::new(());

fn manager_lock() -> MutexGuard<'static, ()> {
	MANAGER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn init_shutdown_roundtrip() {
	let _lock = manager_lock();
	// Ensure a clean slate. The manager tests use the test-only inline
	// backend (the process backend spawns real quercus-worker children).
	RenderManager::shutdown();
	RenderManager::init_with_backend(RenderBackendChoice::Threads).unwrap();
	assert!(RenderManager::global().is_some());
	// Idempotence: second init is a state error.
	assert_eq!(
		RenderManager::init_with_backend(RenderBackendChoice::Threads)
			.unwrap_err()
			.code(),
		Error::State.code()
	);
	RenderManager::shutdown();
	assert!(RenderManager::global().is_none());
	// Re-init works after shutdown (C++ destroy_instance semantics).
	RenderManager::init_with_backend(RenderBackendChoice::Threads).unwrap();
	RenderManager::shutdown();
}

#[test]
fn aggressive_gc_toggle() {
	let _lock = manager_lock();
	RenderManager::shutdown();
	RenderManager::init_with_backend(RenderBackendChoice::Threads).unwrap();
	let m = RenderManager::global().unwrap();
	assert!(!m.aggressive_gc());
	m.set_aggressive_gc(true);
	assert!(m.aggressive_gc());
	RenderManager::shutdown();
}

#[test]
fn cacher_is_lazily_created() {
	let _lock = manager_lock();
	RenderManager::shutdown();
	RenderManager::init_with_backend(RenderBackendChoice::Threads).unwrap();
	let m = RenderManager::global().unwrap();
	{
		let g = m.get_cacher();
		assert!(g.is_some());
	}
	RenderManager::shutdown();
}

#[test]
fn disk_cache_size_and_clear() {
	let _guard = quercus_core::commonutil::ENV_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let dir = std::env::temp_dir().join("quercusrender-diskcache-test");
	std::env::set_var("QUERCUS_CONFIG_DIR", &dir);
	std::fs::create_dir_all(dir.join("mediacache").join("sub")).unwrap();
	std::fs::write(dir.join("mediacache").join("sub").join("a.bin"), [1u8; 100]).unwrap();
	assert_eq!(disk_cache_size().unwrap(), 100);
	disk_cache_clear().unwrap();
	assert_eq!(disk_cache_size().unwrap(), 0);
	assert!(dir.join("mediacache").exists());
	std::fs::remove_dir_all(&dir).ok();
	std::env::remove_var("QUERCUS_CONFIG_DIR");
}

#[test]
fn disk_cache_size_missing_dir_is_zero() {
	let _guard = quercus_core::commonutil::ENV_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let dir = std::env::temp_dir().join("quercusrender-diskcache-missing");
	std::env::set_var("QUERCUS_CONFIG_DIR", &dir);
	let _ = std::fs::remove_dir_all(&dir);
	assert_eq!(disk_cache_size().unwrap(), 0);
	std::env::remove_var("QUERCUS_CONFIG_DIR");
}

#[test]
fn backend_choice_env_selects_pipeline() {
	let _guard = quercus_core::commonutil::ENV_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	std::env::remove_var("QUERCUS_PIPELINE");
	assert!(matches!(
		backend_choice_from_env(),
		RenderBackendChoice::Processes(_)
	));
	std::env::set_var("QUERCUS_PIPELINE", "threads");
	assert!(matches!(
		backend_choice_from_env(),
		RenderBackendChoice::Pipeline
	));
	// Any other value keeps the default process pool.
	std::env::set_var("QUERCUS_PIPELINE", "processes");
	assert!(matches!(
		backend_choice_from_env(),
		RenderBackendChoice::Processes(_)
	));
	std::env::remove_var("QUERCUS_PIPELINE");
}

// ---- disk-cache quota ---------------------------------------------------

/// Serializes the quota tests, which mutate the process-wide
/// `DiskCachePath` / `DiskCacheMaxBytes` config entries. They also hold
/// [`quercus_core::commonutil::ENV_TEST_LOCK`] because the config path
/// depends on `QUERCUS_CONFIG_DIR`.
static DISK_CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn disk_cache_test_lock() -> MutexGuard<'static, ()> {
	DISK_CACHE_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner())
}

/// A unique, empty temp directory for one quota test.
fn quota_temp_dir(tag: &str) -> std::path::PathBuf {
	let dir =
		std::env::temp_dir().join(format!("quercusrender-quota-{}-{tag}", std::process::id()));
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).unwrap();
	dir
}

/// Write `size` zero bytes and sleep briefly so the next write gets a
/// strictly newer mtime (`std` has no portable "set mtime").
fn write_with_mtime_gap(path: &std::path::Path, size: usize) {
	std::fs::write(path, vec![0u8; size]).unwrap();
	std::thread::sleep(std::time::Duration::from_millis(10));
}

/// Sum the regular files under `dir` (mirrors [`disk_cache_size`] but
/// for an explicit directory).
fn dir_file_size(dir: &std::path::Path) -> i64 {
	walk(dir)
		.into_iter()
		.filter_map(|p| std::fs::metadata(&p).ok())
		.filter(|m| m.is_file())
		.map(|m| m.len() as i64)
		.sum()
}

fn save_disk_cache_config() -> (Option<String>, i64) {
	let store = quercus_core::configstore::ConfigStore::instance();
	(
		store.get(None, DISK_CACHE_PATH_KEY).ok(),
		disk_cache_max_bytes(),
	)
}

fn restore_disk_cache_config(saved: (Option<String>, i64)) {
	let store = quercus_core::configstore::ConfigStore::instance();
	store.set(None, DISK_CACHE_PATH_KEY, saved.0.as_deref().unwrap_or(""));
	store.set_int64(None, DISK_CACHE_MAX_BYTES_KEY, saved.1);
	reset_quota_debounce();
}

#[test]
fn prune_dir_removes_oldest_files_first() {
	let dir = quota_temp_dir("oldest");
	for name in ["a", "b", "c", "d"] {
		write_with_mtime_gap(&dir.join(name), 100);
	}
	let freed = prune_dir(&dir, 250).unwrap();
	// 400 bytes -> evict a and b (200 bytes) -> 200 <= 250.
	assert_eq!(freed, 200);
	assert!(!dir.join("a").exists(), "oldest file evicted");
	assert!(!dir.join("b").exists(), "second-oldest evicted");
	assert!(dir.join("c").exists());
	assert!(dir.join("d").exists(), "newest file kept");
	assert_eq!(dir_file_size(&dir), 200);
	std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn prune_dir_keeps_newest_file_over_budget() {
	let dir = quota_temp_dir("newest");
	write_with_mtime_gap(&dir.join("old"), 100);
	write_with_mtime_gap(&dir.join("new"), 100);
	// Budget below a single file: evict everything but the newest.
	assert_eq!(prune_dir(&dir, 10).unwrap(), 100);
	assert!(!dir.join("old").exists());
	assert!(dir.join("new").exists());
	assert_eq!(dir_file_size(&dir), 100);
	// With only the newest left it is retained even though over budget.
	assert_eq!(prune_dir(&dir, 10).unwrap(), 0);
	assert!(dir.join("new").exists());
	std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn prune_dir_zero_or_negative_budget_is_unlimited() {
	let dir = quota_temp_dir("unlimited");
	write_with_mtime_gap(&dir.join("a"), 100);
	write_with_mtime_gap(&dir.join("b"), 100);
	assert_eq!(prune_dir(&dir, 0).unwrap(), 0);
	assert_eq!(prune_dir(&dir, -5).unwrap(), 0);
	assert!(dir.join("a").exists());
	assert!(dir.join("b").exists());
	std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn prune_dir_missing_directory_is_zero() {
	let dir = quota_temp_dir("missing").join("not-there");
	assert_eq!(prune_dir(&dir, 100).unwrap(), 0);
	assert!(!dir.exists());
}

#[test]
fn prune_disk_cache_to_missing_configured_dir_is_zero() {
	let _local = disk_cache_test_lock();
	let _env = quercus_core::commonutil::ENV_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let saved = save_disk_cache_config();
	let dir = quota_temp_dir("configured-missing").join("not-there");
	quercus_core::configstore::ConfigStore::instance().set(
		None,
		DISK_CACHE_PATH_KEY,
		&dir.to_string_lossy(),
	);
	assert_eq!(prune_disk_cache_to(100).unwrap(), 0);
	restore_disk_cache_config(saved);
}

#[test]
fn set_max_bytes_writes_config_and_enforces() {
	let _local = disk_cache_test_lock();
	let _env = quercus_core::commonutil::ENV_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let saved = save_disk_cache_config();
	let dir = quota_temp_dir("config");
	for name in ["a", "b", "c", "d"] {
		write_with_mtime_gap(&dir.join(name), 100);
	}

	set_disk_cache_path(&dir.to_string_lossy()).unwrap();
	assert_eq!(
		disk_cache_path(),
		dir.to_string_lossy().into_owned(),
		"DiskCachePath points at the temp dir"
	);
	assert_eq!(
		disk_cache_size().unwrap(),
		400,
		"budget disabled before set"
	);

	// A zero budget makes enforcement a no-op even after a write event.
	enforce_disk_cache_quota();
	assert_eq!(disk_cache_size().unwrap(), 400);

	set_disk_cache_max_bytes(250).unwrap();
	assert_eq!(disk_cache_max_bytes(), 250);
	assert_eq!(
		quercus_core::configstore::ConfigStore::instance()
			.get(None, DISK_CACHE_MAX_BYTES_KEY)
			.unwrap(),
		"250",
		"config key written"
	);
	assert!(
		disk_cache_size().unwrap() <= 250,
		"setter enforced the quota immediately"
	);
	assert!(!dir.join("a").exists());
	assert!(!dir.join("b").exists());
	assert!(dir.join("c").exists());
	assert!(dir.join("d").exists());

	// Negative values clamp to 0 (unlimited) and disable pruning.
	set_disk_cache_max_bytes(-7).unwrap();
	assert_eq!(disk_cache_max_bytes(), 0);
	assert_eq!(
		quercus_core::configstore::ConfigStore::instance().get_int64(
			None,
			DISK_CACHE_MAX_BYTES_KEY,
			-1
		),
		0
	);

	restore_disk_cache_config(saved);
	std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn set_disk_cache_path_validates_and_creates() {
	let _local = disk_cache_test_lock();
	let _env = quercus_core::commonutil::ENV_TEST_LOCK
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let saved = save_disk_cache_config();
	let base = quota_temp_dir("path");
	let target = base.join("nested").join("cache");

	assert_eq!(
		set_disk_cache_path("   ").unwrap_err().code(),
		Error::Invalid.code()
	);
	assert_eq!(
		set_disk_cache_path("relative/cache").unwrap_err().code(),
		Error::Failed("".into()).code()
	);
	set_disk_cache_path(&target.to_string_lossy()).unwrap();
	assert!(target.is_dir(), "setter creates the directory");
	assert_eq!(disk_cache_path(), target.to_string_lossy().into_owned());

	restore_disk_cache_config(saved);
	std::fs::remove_dir_all(&base).ok();
}
