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

//! Leveled logging, mirroring `src/common/src/debug.h` and
//! `include/common/debug.h`. De-Qt replacement for the old Qt message
//! handler and the qDebug()/qInfo()/qWarning()/qCritical() call sites.
//!
//! Built on the `log` facade crate (crates.io `log`, MIT/Apache-2.0):
//! filtering is `log::set_max_level` plus a mirrored Quercus threshold
//! (needed because the facade has no Fatal severity), and the output
//! dispatcher is a `log::Log` implementation installed on first use.
//!
//! A host can install a process-wide sink with [`set_log_sink`]: the
//! dispatcher then formats `"[LEVEL] message"` and hands it to that safe
//! [`LogSink`] implementation instead of stderr. [`log_is_sink_installed`]
//! reports whether one is active. The C ABI wrappers
//! (`quercus_core_log_set_level`, `quercus_core_log_get_level`,
//! `quercus_core_log_set_sink`, `quercus_core_log_write`) live in
//! `quercus-engine`'s `corestate` module and forward here; the C callback
//! adapter that implements [`LogSink`] lives in the engine's `ext` module.

use std::io::Write;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, Once};

use log::{LevelFilter, Log, Metadata, Record};

/// Discriminant of [`Level::Info`], the default filter level (`k_debug_info`).
const DEFAULT_LOG_LEVEL: Level = Level::Info;

/// Logger installation (first `log`/`log_raw` call).
static LOGGER_INIT: Once = Once::new();

/// Mirror of the last level set through [`log_set_level`]. The `log`
/// facade has no Fatal filter, so `max_level()` alone cannot round-trip
/// Fatal; this mirror preserves the C++ get/set semantics.
static LEVEL_MIRROR: AtomicI32 = AtomicI32::new(DEFAULT_LOG_LEVEL as i32);

/// Record target used to carry [`Level::Fatal`] across the `log` facade,
/// whose severities stop at Error.
const FATAL_TARGET: &str = "quercus_core::debug::fatal";

/// A caller-installable process-wide log sink.
///
/// The dispatcher hands every message that passes the current level to
/// [`LogSink::log`], with `level` as a [`Level`] discriminant (0 = DEBUG …
/// 4 = FATAL) and `message` already formatted as `"[LEVEL] message"`.
pub trait LogSink: Send + Sync {
	/// `message` is already formatted as `"[LEVEL] text"`.
	fn log(&self, level: i32, message: &str);
}

/// The process-wide sink slot. Cloning the `Arc` under the lock lets the
/// dispatcher release the lock before calling the sink, so a sink may log
/// recursively without deadlocking while the implementation stays alive.
static SINK: Mutex<Option<Arc<dyn LogSink>>> = Mutex::new(None);

/// The dispatcher behind the `log` facade: level filtering happens in the
/// facade (and in [`log`]), while this type decides between the installed
/// sink and stderr.
struct DispatcherLogger;

impl DispatcherLogger {
	/// Writes `[LEVEL] message` to the installed sink, or to stderr when
	/// none is installed.
	fn emit(&self, level: Level, message: &str) {
		let line = format!("[{}] {}", level.name(), message);
		// The guard is dropped at the end of this statement; the cloned
		// Arc keeps the sink implementation alive during the call below.
		let sink = SINK.lock().unwrap_or_else(|e| e.into_inner()).clone();
		match sink {
			Some(sink) => {
				// Called outside the lock so the sink may log recursively
				// without deadlocking.
				sink.log(level as i32, &line);
			}
			None => write_stderr(&line),
		}
	}
}

/// Writes one already-formatted line to stderr (the no-sink path).
fn write_stderr(line: &str) {
	let mut err = std::io::stderr();
	// Errors here are swallowed: a logger must not panic the caller.
	let _ = writeln!(err, "{line}");
	let _ = err.flush();
}

impl Log for DispatcherLogger {
	fn enabled(&self, metadata: &Metadata) -> bool {
		metadata.level() <= log::max_level()
	}

	fn log(&self, record: &Record) {
		if !self.enabled(record.metadata()) {
			return;
		}
		let level = if record.target() == FATAL_TARGET {
			Level::Fatal
		} else {
			level_from_facade(record.level())
		};
		self.emit(level, &record.args().to_string());
	}

	fn flush(&self) {
		let _ = std::io::stderr().flush();
	}
}

/// The five Quercus levels mapped onto the `log` facade's five levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
	/// Verbose debug message.
	Debug,
	/// Informational message.
	Info,
	/// Warning message.
	Warning,
	/// Error message.
	Error,
	/// Fatal error message.
	Fatal,
}

impl Level {
	/// One of [`Level`] for an integer code, or `None` out of range.
	///
	/// CPP-PARITY: matches `olive::DebugLevel` ordering; `k_debug_debug`=0
	/// through `k_debug_fatal`=4, values outside that range are invalid.
	pub fn from_code(code: i32) -> Option<Level> {
		match code {
			0 => Some(Level::Debug),
			1 => Some(Level::Info),
			2 => Some(Level::Warning),
			3 => Some(Level::Error),
			4 => Some(Level::Fatal),
			_ => None,
		}
	}

	/// Printable name ("DEBUG", "INFO", ...).
	pub fn name(self) -> &'static str {
		match self {
			Level::Debug => "DEBUG",
			Level::Info => "INFO",
			Level::Warning => "WARNING",
			Level::Error => "ERROR",
			Level::Fatal => "FATAL",
		}
	}

	/// Facade filter level.
	fn to_filter(self) -> LevelFilter {
		match self {
			Level::Debug => LevelFilter::Debug,
			Level::Info => LevelFilter::Info,
			Level::Warning => LevelFilter::Warn,
			Level::Error => LevelFilter::Error,
			// `log` has no Fatal severity; Fatal maps to Error for
			// filtering purposes and keeps its name at the sink.
			Level::Fatal => LevelFilter::Error,
		}
	}

	/// Facade record level.
	fn to_log_level(self) -> log::Level {
		match self {
			Level::Debug => log::Level::Debug,
			Level::Info => log::Level::Info,
			Level::Warning => log::Level::Warn,
			Level::Error | Level::Fatal => log::Level::Error,
		}
	}
}

/// Quercus level for a facade level (records not emitted by [`log`]).
/// `log` has no Fatal; Trace keeps the FATAL tag the old stderr logger
/// printed for it.
fn level_from_facade(level: log::Level) -> Level {
	match level {
		log::Level::Debug => Level::Debug,
		log::Level::Info => Level::Info,
		log::Level::Warn => Level::Warning,
		log::Level::Error => Level::Error,
		log::Level::Trace => Level::Fatal,
	}
}

/// Install the dispatcher once and apply the default filter.
fn ensure_logger() {
	LOGGER_INIT.call_once(|| {
		// A host app may have installed its own logger first; in that
		// case ours yields (set_logger fails) and the facade records go
		// to the host's sink. Filtering still runs through max_level.
		let _ = log::set_logger(&DispatcherLogger);
		log::set_max_level(DEFAULT_LOG_LEVEL.to_filter());
	});
}

/// Install, replace, or clear (`sink` = `None`) the process-wide log sink.
///
/// Once installed, every message that passes the current level is
/// formatted as `"[LEVEL] message"` and handed to `sink` (called outside
/// the internal lock, so a sink may log recursively). The previous sink's
/// `Arc` is swapped out under the lock and dropped outside it, so its
/// `Drop` implementation (for a C adapter, the host drop callback) may
/// itself log or install another sink.
pub fn set_log_sink(sink: Option<Arc<dyn LogSink>>) {
	ensure_logger();
	// Swap under the lock, then drop the old sink outside it (the sink's
	// Drop may itself log or install another sink).
	let old = {
		let mut slot = SINK.lock().unwrap_or_else(|e| e.into_inner());
		std::mem::replace(&mut *slot, sink)
	};
	std::mem::drop(old);
}

/// Whether a sink callback is currently installed.
pub fn log_is_sink_installed() -> bool {
	SINK.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Current minimum level emitted by [`log`]; the default is [`Level::Info`].
pub fn log_get_level() -> Level {
	ensure_logger();
	Level::from_code(LEVEL_MIRROR.load(Ordering::Relaxed)).unwrap_or(Level::Info)
}

/// Set the minimum level emitted by [`log`].
pub fn log_set_level(level: Level) {
	ensure_logger();
	LEVEL_MIRROR.store(level as i32, Ordering::Relaxed);
	log::set_max_level(level.to_filter());
}

/// Emit `msg` at `level` (below-threshold messages are dropped, C++
/// `log_message` semantics). Only messages that pass the current level
/// reach an installed sink.
pub fn log(level: Level, msg: &str) -> crate::error::Result<()> {
	ensure_logger();
	// The facade cannot express Fatal-only filtering (Error is its highest
	// floor), so honor the mirrored Quercus threshold here as well.
	if (level as i32) < LEVEL_MIRROR.load(Ordering::Relaxed) {
		return Ok(());
	}
	if level == Level::Fatal {
		// Fatal must survive the facade's Error floor; the target tells the
		// dispatcher to tag it FATAL.
		log::log!(target: FATAL_TARGET, log::Level::Error, "{}", msg);
	} else {
		log::log!(level.to_log_level(), "{}", msg);
	}
	Ok(())
}

/// Emit `msg` unconditionally, prefixing `level` ("UNKNOWN" for
/// out-of-range codes).
pub fn log_raw(level: i32, msg: &str) -> crate::error::Result<()> {
	match Level::from_code(level) {
		Some(l) => log(l, msg),
		None => {
			ensure_logger();
			// Unknown codes have no sink level code: bypass both the facade
			// and any sink and write the raw line (C++ prints them with an
			// UNKNOWN tag).
			write_stderr(&format!("[UNKNOWN] {msg}"));
			Ok(())
		}
	}
}

#[cfg(test)]
mod tests {
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
}
