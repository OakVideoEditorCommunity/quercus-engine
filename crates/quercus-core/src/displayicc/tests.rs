use super::*;
use std::sync::{Mutex, MutexGuard};

// Env-mutating tests serialize on the crate-wide test lock (shared with
// the configstore/filefunctions tests, which also touch `QUERCUS_CONFIG_DIR`).
fn env_lock() -> &'static Mutex<()> {
	crate::test_support::env_lock()
}

fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
	let dir = std::env::temp_dir().join(format!(
		"quercus-displayicc-{}-{}-{}",
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
fn parse_xprop_hex_normal() {
	let out = "_ICC_PROFILE(8)\t= 0x3c, 0x6f, 0x6f, 0x0";
	assert_eq!(parse_xprop_icc_hex(out), Some(vec![0x3c, 0x6f, 0x6f, 0x0]));
}

#[test]
fn parse_xprop_hex_suffixed_name() {
	// Multi-monitor X servers name extra properties `_ICC_PROFILE_1`,
	// `_ICC_PROFILE_2`, ...; the byte list format is unchanged.
	let out = "_ICC_PROFILE_1(8)\t= 0x00, 0x01";
	assert_eq!(parse_xprop_icc_hex(out), Some(vec![0x00, 0x01]));
}

#[test]
fn parse_xprop_hex_empty_and_absent() {
	// xprop prints an error to stderr for a missing property...
	assert_eq!(parse_xprop_icc_hex("xprop:  error: Invalid atom"), None);
	// ...and an existing-but-empty property has nothing to parse.
	assert_eq!(parse_xprop_icc_hex("_ICC_PROFILE(8)\t= "), None);
	assert_eq!(parse_xprop_icc_hex(""), None);
}

#[test]
fn parse_xprop_hex_malformed() {
	// Invalid `0x` tokens are skipped, valid neighbours still parse.
	assert_eq!(
		parse_xprop_icc_hex("_ICC_PROFILE(8)\t= 0xZZ, 0x3c, 0xGG"),
		Some(vec![0x3c])
	);
	// A lone `0x` prefix contributes no byte.
	assert_eq!(parse_xprop_icc_hex("_ICC_PROFILE(8)\t= 0x"), None);
	// No `=` separator at all.
	assert_eq!(parse_xprop_icc_hex("0x3c 0x6f"), None);
	// `0X` prefixes and uppercase digits are accepted.
	assert_eq!(
		parse_xprop_icc_hex("_ICC_PROFILE(8)\t= 0X3C, 0X6f"),
		Some(vec![0x3c, 0x6f])
	);
	// Bare hex without an `0x` prefix is not a byte token.
	assert_eq!(parse_xprop_icc_hex("_ICC_PROFILE(8)\t= 3c, 6f"), None);
}

#[test]
fn env_override_wins() {
	let _guard: MutexGuard<()> = env_lock().lock().unwrap();
	let dir = unique_temp_dir("override");
	let profile = dir.join("fake.icc");
	std::fs::write(&profile, b"fake").unwrap();
	std::env::set_var("QUERCUS_DISPLAY_ICC", &profile);
	let got = system_display_icc();
	std::env::remove_var("QUERCUS_DISPLAY_ICC");
	assert_eq!(got, Some(profile.to_string_lossy().into_owned()));
}

#[test]
fn empty_override_falls_through() {
	let _guard: MutexGuard<()> = env_lock().lock().unwrap();
	std::env::set_var("QUERCUS_DISPLAY_ICC", "");
	let with_empty = system_display_icc();
	std::env::remove_var("QUERCUS_DISPLAY_ICC");
	let without = system_display_icc();
	assert_eq!(with_empty, without);
}

#[test]
fn platform_query_does_not_panic() {
	// Smoke test for the platform FFI path: it must return without
	// panicking, and any path it yields must be non-empty. A headless
	// environment legitimately produces None.
	if let Some(path) = system_display_icc() {
		assert!(!path.is_empty());
	}
}

#[cfg(not(target_os = "windows"))]
#[test]
fn icc_cache_roundtrip_and_dedup() {
	let _guard: MutexGuard<()> = env_lock().lock().unwrap();
	let dir = unique_temp_dir("cache");
	std::env::set_var("QUERCUS_CONFIG_DIR", &dir);
	let bytes = b"\xacsp sRGB IEC61966-2.1 profile bytes".to_vec();

	let p1 = write_icc_cache(&bytes).expect("cache write");
	let p2 = write_icc_cache(&bytes).expect("cache write");
	assert_eq!(p1, p2, "identical content reuses the same file");
	assert_eq!(std::fs::read(&p1).unwrap(), bytes);
	assert!(p1.ends_with(".icc"));
	// The file name embeds the 16-hex-digit content hash.
	let stem = Path::new(&p1).file_stem().unwrap().to_str().unwrap();
	assert_eq!(stem.len(), 16);
	assert!(stem.chars().all(|c| c.is_ascii_hexdigit()));
	// The cache lives under the configured location.
	assert!(Path::new(&p1).starts_with(&dir));
	// Different content -> different file.
	let p3 = write_icc_cache(b"other bytes").unwrap();
	assert_ne!(p1, p3);

	std::env::remove_var("QUERCUS_CONFIG_DIR");
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parse_xrandr_monitors_two_screens() {
	let out = "\
Monitors: 2
 0: +*eDP-1 1920/344x1080/194+0+0  eDP-1
 1: +HDMI-1 3840/600x2160/340+1920+0  HDMI-1
";
	let monitors = parse_xrandr_monitors(out);
	assert_eq!(monitors.len(), 2);
	assert_eq!(monitors[0].x, 0);
	assert_eq!(monitors[0].y, 0);
	assert_eq!(monitors[0].width, 1920);
	assert_eq!(monitors[0].height, 1080);
	assert_eq!(monitors[0].outputs, ["eDP-1"]);
	assert_eq!(monitors[1].x, 1920);
	assert_eq!(monitors[1].y, 0);
	assert_eq!(monitors[1].width, 3840);
	assert_eq!(monitors[1].height, 2160);
	assert_eq!(monitors[1].outputs, ["HDMI-1"]);
}

#[test]
fn parse_xrandr_monitors_negative_coords_and_spanning() {
	// A monitor left of the primary has a negative X11 coordinate,
	// printed with a `-` prefix.
	let out = "\
Monitors: 2
 0: +*DP-1 1080/293x1920/509+0+0  DP-1
 1: +DP-2 1920/509x1080/293-1080+0  DP-2
";
	let monitors = parse_xrandr_monitors(out);
	assert_eq!(monitors[1].x, -1080);
	assert_eq!(monitors[1].y, 0);
	// A spanning mode lists several outputs on one monitor.
	let out = "\
Monitors: 1
 0: +*eDP-1 3840/700x1080/194+0+0  eDP-1 HDMI-1
";
	let monitors = parse_xrandr_monitors(out);
	assert_eq!(monitors.len(), 1);
	assert_eq!(monitors[0].outputs, ["eDP-1", "HDMI-1"]);
}

#[test]
fn parse_xrandr_monitors_malformed() {
	assert_eq!(parse_xrandr_monitors(""), Vec::<RandRMonitor>::new());
	assert_eq!(
		parse_xrandr_monitors("Monitors: 0\n"),
		Vec::<RandRMonitor>::new()
	);
	// No `:` separator, a missing geometry, and a geometry with no
	// outputs are all skipped without panicking.
	let out = "\
Monitors: 2
 garbage line
 0: +*eDP-1  eDP-1
 1: +HDMI-1 1920/344x1080/194+1920+0
 2: +VGA-1 1024/200x768/150+0+0  VGA-1
";
	let monitors = parse_xrandr_monitors(out);
	assert_eq!(monitors.len(), 1);
	assert_eq!(monitors[0].outputs, ["VGA-1"]);
}

#[test]
fn parse_output_icc_prop_hit_and_miss() {
	let out = "\
Screen 0: minimum 320 x 200, current 3840 x 1080, maximum 16384 x 16384
eDP-1 connected primary 1920x1080+0+0 (normal left inverted right x axis y axis) 344mm x 194mm
\t_ICC_PROFILE(8)\t= 0x3c, 0x6f, 0x6f
\tEDID(0)\t= 0x00, 0xff, 0xff, 0xff
HDMI-1 connected 1920x1080+1920+0 (normal left inverted right x axis y axis) 600mm x 340mm
\t_ICC_PROFILE(8)\t= 0x41, 0x42
";
	assert_eq!(
		parse_output_icc_prop(out, "eDP-1"),
		Some(vec![0x3c, 0x6f, 0x6f])
	);
	assert_eq!(parse_output_icc_prop(out, "HDMI-1"), Some(vec![0x41, 0x42]));
	// An output not present in the list.
	assert_eq!(parse_output_icc_prop(out, "VGA-1"), None);
}

#[test]
fn parse_output_icc_prop_without_profile() {
	// An output whose section carries no `_ICC_PROFILE` yields None, and
	// so does a malformed property value.
	let out = "\
DP-1 connected 1920x1080+0+0 (normal left inverted right x axis y axis) 344mm x 194mm
\tEDID(0)\t= 0x00, 0xff
";
	assert_eq!(parse_output_icc_prop(out, "DP-1"), None);
	let out = "\
DP-1 connected 1920x1080+0+0 (normal left inverted right x axis y axis) 344mm x 194mm
\t_ICC_PROFILE(8)\t= 0x
";
	assert_eq!(parse_output_icc_prop(out, "DP-1"), None);
}

#[test]
fn monitor_ref_fingerprint_roundtrip() {
	assert_eq!(
		monitor_ref_from_fingerprint("mac:4294967295"),
		Some(MonitorRef::MacDisplay(u32::MAX))
	);
	assert_eq!(
		monitor_ref_from_fingerprint(r"win:\\.\DISPLAY1"),
		Some(MonitorRef::WinDevice(r"\\.\DISPLAY1".to_string()))
	);
	assert_eq!(
		monitor_ref_from_fingerprint("x11:eDP-1"),
		Some(MonitorRef::X11Output("eDP-1".to_string()))
	);
	// Malformed input.
	assert_eq!(monitor_ref_from_fingerprint(""), None);
	assert_eq!(monitor_ref_from_fingerprint("nope"), None);
	assert_eq!(monitor_ref_from_fingerprint("mac:abc"), None);
	assert_eq!(monitor_ref_from_fingerprint("win:"), None);
	assert_eq!(monitor_ref_from_fingerprint("x11:"), None);
	assert_eq!(monitor_ref_from_fingerprint("os:whatever"), None);
}
