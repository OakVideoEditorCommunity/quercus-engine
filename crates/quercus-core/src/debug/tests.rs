use super::*;

/// Serializes every test that touches the process-global level or sink.
static TEST_LOCK: Mutex<()> = Mutex::new(());
/// Number of times a [`RecordingSink`] has been dropped. Reset by the
/// replacement/clear test, which asserts the count.
static DROP_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn test_lock() -> std::sync::MutexGuard<'static, ()> {
	TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Safe mock sink: records the `(level, formatted message)` pairs it
/// receives and counts its own drops.
struct RecordingSink(Arc<Mutex<Vec<(i32, String)>>>);

impl LogSink for RecordingSink {
	fn log(&self, level: i32, message: &str) {
		self.0
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.push((level, message.to_string()));
	}
}

impl Drop for RecordingSink {
	fn drop(&mut self) {
		DROP_COUNT.fetch_add(1, Ordering::SeqCst);
	}
}

/// Installs a fresh [`RecordingSink`] and returns its record buffer.
fn install_recording_sink() -> Arc<Mutex<Vec<(i32, String)>>> {
	let captured = Arc::new(Mutex::new(Vec::new()));
	set_log_sink(Some(Arc::new(RecordingSink(captured.clone()))));
	captured
}

#[test]
fn level_from_code_valid() {
	assert_eq!(Level::from_code(0), Some(Level::Debug));
	assert_eq!(Level::from_code(1), Some(Level::Info));
	assert_eq!(Level::from_code(2), Some(Level::Warning));
	assert_eq!(Level::from_code(3), Some(Level::Error));
	assert_eq!(Level::from_code(4), Some(Level::Fatal));
}

#[test]
fn level_from_code_out_of_range() {
	assert_eq!(Level::from_code(-1), None);
	assert_eq!(Level::from_code(5), None);
	assert_eq!(Level::from_code(i32::MIN), None);
	assert_eq!(Level::from_code(i32::MAX), None);
}

#[test]
fn level_name() {
	assert_eq!(Level::Debug.name(), "DEBUG");
	assert_eq!(Level::Info.name(), "INFO");
	assert_eq!(Level::Warning.name(), "WARNING");
	assert_eq!(Level::Error.name(), "ERROR");
	assert_eq!(Level::Fatal.name(), "FATAL");
}

#[test]
fn level_discriminants_match_cpp_order() {
	// The enum must order by ascending severity so threshold
	// filtering works, exactly as `olive::DebugLevel`.
	assert!((Level::Debug as i32) < (Level::Info as i32));
	assert!((Level::Info as i32) < (Level::Warning as i32));
	assert!((Level::Warning as i32) < (Level::Error as i32));
	assert!((Level::Error as i32) < (Level::Fatal as i32));
}

#[test]
fn default_level_is_info() {
	let _guard = test_lock();
	// The documented default is Info; (re)applying it must drive the
	// process-global filter to the Info floor.
	assert_eq!(DEFAULT_LOG_LEVEL, Level::Info);
	log_set_level(DEFAULT_LOG_LEVEL);
	assert_eq!(log_get_level(), Level::Info);
	assert_eq!(log::max_level(), DEFAULT_LOG_LEVEL.to_filter());
}

#[test]
fn set_get_level_round_trip() {
	let _guard = test_lock();
	for level in [
		Level::Debug,
		Level::Info,
		Level::Warning,
		Level::Error,
		Level::Fatal,
	] {
		log_set_level(level);
		assert_eq!(log_get_level(), level);
	}
	log_set_level(Level::Info); // restore default for other tests
}

#[test]
fn log_returns_ok() {
	let _guard = test_lock();
	log_set_level(Level::Error);
	assert!(log(Level::Info, "below threshold").is_ok());
	assert!(log(Level::Error, "at threshold").is_ok());
	assert!(log(Level::Fatal, "above threshold").is_ok());
	log_set_level(Level::Info);
}

#[test]
fn log_raw_returns_ok_for_any_level() {
	let _guard = test_lock();
	for code in [-1, 0, 4, 5, i32::MIN, i32::MAX] {
		assert!(log_raw(code, "raw message").is_ok());
	}
}

#[test]
fn filtering_drops_below_threshold() {
	let _guard = test_lock();
	for threshold in [
		Level::Debug,
		Level::Info,
		Level::Warning,
		Level::Error,
		Level::Fatal,
	] {
		log_set_level(threshold);
		// The observable filter state: the facade's max level tracks
		// the threshold, and below-threshold traffic is disabled.
		assert_eq!(log::max_level(), threshold.to_filter());
		if threshold != Level::Debug {
			assert!(
				!log::log_enabled!(log::Level::Debug),
				"debug traffic must be filtered at {threshold:?}"
			);
		}
		// Emitting below the threshold is still an Ok no-op.
		assert!(log(Level::Debug, "x").is_ok());
		assert!(log(Level::Fatal, "y").is_ok());
	}
	log_set_level(Level::Info);
}

#[test]
fn sink_receives_only_messages_that_pass_the_level() {
	let _guard = test_lock();
	set_log_sink(None);
	log_set_level(Level::Warning);
	let captured = install_recording_sink();
	assert!(log_is_sink_installed());

	assert!(log(Level::Debug, "debug").is_ok());
	assert!(log(Level::Info, "info").is_ok());
	assert!(log(Level::Warning, "warn").is_ok());
	assert!(log(Level::Error, "error").is_ok());
	assert!(log(Level::Fatal, "fatal").is_ok());

	// The sink sees the formatted line and the level code, and only
	// messages at or above the Warning threshold.
	assert_eq!(
		captured.lock().unwrap_or_else(|e| e.into_inner()).clone(),
		vec![
			(2, "[WARNING] warn".to_string()),
			(3, "[ERROR] error".to_string()),
			(4, "[FATAL] fatal".to_string()),
		]
	);

	// Fatal-only threshold: Error is below it and must not leak through.
	log_set_level(Level::Fatal);
	captured.lock().unwrap_or_else(|e| e.into_inner()).clear();
	assert!(log(Level::Error, "below fatal").is_ok());
	assert!(log(Level::Fatal, "fatal again").is_ok());
	assert_eq!(
		captured.lock().unwrap_or_else(|e| e.into_inner()).clone(),
		vec![(4, "[FATAL] fatal again".to_string())]
	);

	set_log_sink(None);
	assert!(!log_is_sink_installed());
	log_set_level(Level::Info);
}

#[test]
fn replacing_or_clearing_drops_the_old_sink_exactly_once() {
	let _guard = test_lock();
	set_log_sink(None);
	DROP_COUNT.store(0, Ordering::SeqCst);

	let first = Arc::new(Mutex::new(Vec::new()));
	set_log_sink(Some(Arc::new(RecordingSink(first.clone()))));
	assert_eq!(DROP_COUNT.load(Ordering::SeqCst), 0);

	// Replace: the previous sink is dropped exactly once.
	let second = Arc::new(Mutex::new(Vec::new()));
	set_log_sink(Some(Arc::new(RecordingSink(second.clone()))));
	assert_eq!(DROP_COUNT.load(Ordering::SeqCst), 1);

	// Clear: the current sink is dropped once more.
	set_log_sink(None);
	assert_eq!(DROP_COUNT.load(Ordering::SeqCst), 2);
	assert!(!log_is_sink_installed());
}

#[test]
fn clearing_restores_stderr_and_stops_sink_capture() {
	let _guard = test_lock();
	set_log_sink(None);
	log_set_level(Level::Info);
	let captured = install_recording_sink();
	assert!(log(Level::Error, "captured").is_ok());
	assert_eq!(captured.lock().unwrap_or_else(|e| e.into_inner()).len(), 1);

	set_log_sink(None);
	assert!(!log_is_sink_installed());
	// Back on the stderr path: the cleared sink must not be called.
	assert!(log(Level::Error, "to stderr").is_ok());
	assert_eq!(
		captured.lock().unwrap_or_else(|e| e.into_inner()).len(),
		1,
		"cleared sink must not be called"
	);
}
