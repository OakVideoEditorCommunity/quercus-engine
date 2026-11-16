use std::ffi::c_void;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use super::*;

#[test]
fn decibel_from_linear_known_values() {
	assert_eq!(decibel_from_linear(1.0).unwrap(), 0.0);
	assert!((decibel_from_linear(10.0).unwrap() - 20.0).abs() < 1e-12);
	assert!((decibel_from_linear(100.0).unwrap() - 40.0).abs() < 1e-12);
	// Zero / negative-infinite log10 clamps to minimum.
	assert_eq!(decibel_from_linear(0.0).unwrap(), DECIBEL_MINIMUM);
	// Negative input yields NaN in both C++ and Rust (no clamp).
	assert!(decibel_from_linear(-1.0).unwrap().is_nan());
}

#[test]
fn decibel_to_linear_known_values() {
	assert_eq!(decibel_to_linear(0.0).unwrap(), 1.0);
	assert!((decibel_to_linear(20.0).unwrap() - 10.0).abs() < 1e-12);
	// Well below -120 dB clamps to 0; exactly 1e-6 does not.
	assert_eq!(decibel_to_linear(-200.0).unwrap(), 0.0);
	assert_eq!(decibel_to_linear(-120.0).unwrap(), 0.000001);
}

#[test]
fn decibel_logarithmic_branch_thresholds() {
	// Very small positions clamp to minimum; >0.99 clamp to 0 dB.
	assert_eq!(decibel_from_logarithmic(0.0).unwrap(), DECIBEL_MINIMUM);
	assert_eq!(decibel_from_logarithmic(1.0).unwrap(), 0.0);
	// Mid-range is a real value.
	let db = decibel_from_logarithmic(0.5).unwrap();
	assert!(db.is_finite());
	// to_logarithmic clamps |db| <= 1e-12 to 1.0.
	assert_eq!(decibel_to_logarithmic(0.0).unwrap(), 1.0);
}

#[test]
fn decibel_round_trips() {
	// linear <-> logarithmic round trip (within float tolerance).
	for linear in [0.001, 0.01, 0.1, 0.5, 0.9, 0.99] {
		let log = decibel_linear_to_logarithmic(linear).unwrap();
		let back = decibel_logarithmic_to_linear(log).unwrap();
		assert!(
			(back - linear).abs() < 1e-6,
			"linear {} -> {} -> {}",
			linear,
			log,
			back
		);
	}
	// db <-> logarithmic round trip. Only non-positive db are reversible:
	// a positive db pushes the logarithmic position past 0.99, which the
	// C++ `from_logarithmic` intentionally clamps back to 0 dB.
	for db in [-60.0, -30.0, -12.0, -6.0, -3.0, -1.0] {
		let log = decibel_to_logarithmic(db).unwrap();
		let back = decibel_from_logarithmic(log).unwrap();
		assert!((back - db).abs() < 1e-3, "db {} -> {} -> {}", db, log, back);
	}
	// A positive db saturates the logarithmic position > 0.99 and comes
	// back as 0 dB (faithful to the C++ clamp).
	let log6 = decibel_to_logarithmic(6.0).unwrap();
	assert!(log6 > 0.99);
	assert_eq!(decibel_from_logarithmic(log6).unwrap(), 0.0);
	// logarithmic_to_linear clamps >0.99 to 1.
	assert_eq!(decibel_logarithmic_to_linear(0.999).unwrap(), 1.0);
}

#[test]
fn lerp_matches_cpp() {
	assert_eq!(lerp(0.0, 10.0, 0.0).unwrap(), 0.0);
	assert_eq!(lerp(0.0, 10.0, 1.0).unwrap(), 10.0);
	assert_eq!(lerp(0.0, 10.0, 0.5).unwrap(), 5.0);
	assert_eq!(lerp(2.0, 4.0, 0.25).unwrap(), 2.5);
}

#[test]
fn loop_mode_discriminants_match_cpp() {
	// Values are load-bearing across the C ABI (include/common/loopmode.h
	// and src/common/src/loopmode.h).
	assert_eq!(LoopMode::Off as i32, 0);
	assert_eq!(LoopMode::Loop as i32, 1);
	assert_eq!(LoopMode::Clamp as i32, 2);
	// Copy/Clone/Eq semantics of a plain enum.
	let a = LoopMode::Loop;
	let b = a;
	assert_eq!(a, b);
	assert_ne!(LoopMode::Off, LoopMode::Clamp);
}

#[test]
fn drop_workflow_behavior_discriminants_match_cpp() {
	// The config layer persists these as ints (enum QuercusDropWorkflowBehavior).
	assert_eq!(DropWorkflowBehavior::Ask as i32, 0);
	assert_eq!(DropWorkflowBehavior::Auto as i32, 1);
	assert_eq!(DropWorkflowBehavior::Manual as i32, 2);
	assert_eq!(DropWorkflowBehavior::Disable as i32, 3);
}

#[test]
fn drop_workflow_behavior() {
	for (v, expected_valid) in [(0, true), (1, true), (2, true), (3, true)] {
		assert_eq!(DropWorkflowBehavior::is_valid(v), expected_valid);
	}
	assert!(!DropWorkflowBehavior::is_valid(-1));
	assert!(!DropWorkflowBehavior::is_valid(4));

	assert_eq!(DropWorkflowBehavior::name(0), "ASK");
	assert_eq!(DropWorkflowBehavior::name(1), "AUTO");
	assert_eq!(DropWorkflowBehavior::name(2), "MANUAL");
	assert_eq!(DropWorkflowBehavior::name(3), "DISABLE");
	assert_eq!(DropWorkflowBehavior::name(99), "UNKNOWN");
	assert_eq!(DropWorkflowBehavior::name(-5), "UNKNOWN");
}

#[test]
fn power_of_two() {
	// ceil
	assert_eq!(power_ceil_to_power_of_2(1).unwrap(), 1);
	assert_eq!(power_ceil_to_power_of_2(2).unwrap(), 2);
	assert_eq!(power_ceil_to_power_of_2(3).unwrap(), 4);
	assert_eq!(power_ceil_to_power_of_2(5).unwrap(), 8);
	assert_eq!(power_ceil_to_power_of_2(8).unwrap(), 8);
	assert_eq!(power_ceil_to_power_of_2(0).unwrap(), 0);
	// floor
	assert_eq!(power_floor_to_power_of_2(1).unwrap(), 1);
	assert_eq!(power_floor_to_power_of_2(2).unwrap(), 2);
	assert_eq!(power_floor_to_power_of_2(5).unwrap(), 4);
	assert_eq!(power_floor_to_power_of_2(9).unwrap(), 8);
	assert_eq!(power_floor_to_power_of_2(8).unwrap(), 8);
	assert_eq!(power_floor_to_power_of_2(0).unwrap(), 0);
	// large value: 0x8000_0001 saturates the bit-blast to u32::MAX, then
	// the final increment wraps to 0 (matching C++ unsigned overflow).
	assert_eq!(power_ceil_to_power_of_2(0x8000_0001).unwrap(), 0u32);
	assert_eq!(power_floor_to_power_of_2(0x8000_0000).unwrap(), 0x8000_0000);
}

/// Per-process destroy counter used by the Current singleton test.
static DESTROY_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Serialises tests that touch the process-wide `Current` singleton
/// (cargo runs tests on threads).
static CURRENT_LOCK: Mutex<()> = Mutex::new(());

/// A `DestroyFn` that bumps [`DESTROY_COUNT`].
unsafe extern "C" fn count_destroy(_p: *mut c_void) {
	DESTROY_COUNT.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn current_slot_semantics() {
	let _guard = CURRENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	// Only the video-params slot is used here; the singleton is shared
	// across tests, so keep each test on its own slot to avoid races.
	let cur = Current::instance();
	assert!(cur.is_interactive().unwrap());

	DESTROY_COUNT.store(0, Ordering::SeqCst);

	// Empty initially.
	assert!(cur.get_video_params().unwrap().is_null());

	// Store with a destructor.
	let p1 = std::ptr::dangling_mut::<c_void>();
	assert!(cur.set_video_params(p1, Some(count_destroy)).is_ok());
	assert_eq!(cur.get_video_params().unwrap(), p1);

	// Replacing destroys the previous occupant.
	let p2 = 0x2 as *mut c_void;
	assert!(cur.set_video_params(p2, Some(count_destroy)).is_ok());
	assert_eq!(cur.get_video_params().unwrap(), p2);
	assert_eq!(DESTROY_COUNT.load(Ordering::SeqCst), 1);

	// Storing NULL clears and destroys the prior occupant.
	assert!(cur.set_video_params(std::ptr::null_mut(), None).is_ok());
	assert!(cur.get_video_params().unwrap().is_null());
	assert_eq!(DESTROY_COUNT.load(Ordering::SeqCst), 2);

	// Clearing when the slot is already empty does nothing.
	assert!(cur.set_video_params(std::ptr::null_mut(), None).is_ok());
	assert_eq!(DESTROY_COUNT.load(Ordering::SeqCst), 2);
}

#[test]
fn current_slots_independent() {
	let _guard = CURRENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let cur = Current::instance();
	let pa = 0x10 as *mut c_void;
	let ph = 0x20 as *mut c_void;
	let pc = 0x30 as *mut c_void;
	// Each slot is independent; no cross-slot interference.
	assert!(cur.set_audio_params(pa, None).is_ok());
	assert!(cur.set_plugin_host(ph, None).is_ok());
	assert!(cur.set_plugin_cache(pc, None).is_ok());
	assert_eq!(cur.get_audio_params().unwrap(), pa);
	assert_eq!(cur.get_plugin_host().unwrap(), ph);
	assert_eq!(cur.get_plugin_cache().unwrap(), pc);
}
