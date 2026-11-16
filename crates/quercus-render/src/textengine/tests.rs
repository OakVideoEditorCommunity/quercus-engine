use super::*;
use std::sync::Mutex as StdMutex;

/// The hook statics are process-global; the one test that installs them
/// holds this so it cannot race a future one.
static HOOK_LOCK: StdMutex<()> = StdMutex::new(());

/// Serializes the tests that extend the process-wide font database or
/// observe its family cache: an add is only visible through the cache
/// transition they check between their own calls.
static FONT_DB_LOCK: StdMutex<()> = StdMutex::new(());

/// A unique scratch directory under the system temp dir, removed on drop.
struct TempDir(std::path::PathBuf);

impl TempDir {
	fn new(tag: &str) -> Self {
		use std::sync::atomic::{AtomicUsize, Ordering};
		static COUNTER: AtomicUsize = AtomicUsize::new(0);
		let path = std::env::temp_dir().join(format!(
			"quercus-textengine-{tag}-{}-{}",
			std::process::id(),
			COUNTER.fetch_add(1, Ordering::Relaxed)
		));
		std::fs::create_dir_all(&path).expect("create the temp dir");
		Self(path)
	}

	fn path(&self) -> &Path {
		&self.0
	}
}

impl Drop for TempDir {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}

/// The number of faces in the process-wide font database.
fn face_count() -> usize {
	lock_system().font_system.db().faces().count()
}

/// The first real font file under the usual system font roots, with its
/// bytes; `None` when this machine has no `.ttf` / `.otf` font.
fn system_font() -> Option<(std::path::PathBuf, Vec<u8>)> {
	fn scan(dir: &Path, depth: usize) -> Option<std::path::PathBuf> {
		if depth > 8 {
			return None;
		}
		for entry in std::fs::read_dir(dir).ok()?.flatten() {
			let path = entry.path();
			if path.is_dir() {
				if let Some(font) = scan(&path, depth + 1) {
					return Some(font);
				}
				continue;
			}
			let extension = path
				.extension()
				.and_then(|e| e.to_str())
				.map(|e| e.to_ascii_lowercase());
			if matches!(extension.as_deref(), Some("ttf") | Some("otf")) {
				return Some(path);
			}
		}
		None
	}

	["/usr/share/fonts", "/usr/local/share/fonts"]
		.iter()
		.find_map(|root| {
			let path = scan(Path::new(root), 0)?;
			let data = std::fs::read(&path).ok()?;
			(!data.is_empty()).then_some((path, data))
		})
}

/// Asserts that `families` is sorted and free of duplicates.
fn assert_sorted_and_unique(families: &[String]) {
	let mut sorted = families.to_vec();
	sorted.sort();
	sorted.dedup();
	assert_eq!(families, sorted);
}

/// A plain-text request with the test defaults (72 pt at 3780
/// dots/meter, no wrap, no centering, backend default family).
fn plain(text: &str, font_size_pt: f64, wrap_width: f64) -> TextLayoutRequest {
	TextLayoutRequest {
		text: text.to_string(),
		mode: TextLayoutMode::PlainText,
		font_family: String::new(),
		font_size_pt,
		dots_per_meter: 3780,
		wrap_width,
		center_horizontally: false,
	}
}

/// A transform with only the scale set (the rest neutral).
fn scaled(scale: f64) -> TextRenderTransform {
	TextRenderTransform {
		scale,
		..TextRenderTransform::default()
	}
}

/// Renders a request into a zeroed buffer and returns it.
fn render_into(
	req: &TextLayoutRequest,
	transform: &TextRenderTransform,
	width: i32,
	height: i32,
	channel_count: i32,
) -> Vec<u8> {
	let mut data = vec![0u8; (width * height * channel_count) as usize];
	render(
		req,
		transform,
		TextRenderTarget {
			data: &mut data,
			width,
			height,
			linesize_bytes: width * channel_count,
			channel_count,
		},
	);
	data
}

/// Bounding box (`min_x`, `min_y`, `max_x`, `max_y`) of the nonzero
/// pixels of a grayscale buffer; `None` when nothing was painted.
fn ink_bounds(data: &[u8], width: i32, height: i32) -> Option<(i32, i32, i32, i32)> {
	let mut bounds: Option<(i32, i32, i32, i32)> = None;
	for y in 0..height {
		for x in 0..width {
			if data[(y * width + x) as usize] != 0 {
				bounds = Some(match bounds {
					None => (x, y, x, y),
					Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
				});
			}
		}
	}
	bounds
}

fn has_ink(data: &[u8]) -> bool {
	data.iter().any(|b| *b != 0)
}

#[test]
fn plain_text_measures_nonzero() {
	let size = measure(&plain("Hello", 72.0, 0.0));
	assert!(size.width > 0.0, "width = {}", size.width);
	assert!(size.height > 0.0, "height = {}", size.height);
}

#[test]
fn measure_scales_with_font_size() {
	let small = measure(&plain("Hello", 36.0, 0.0));
	let large = measure(&plain("Hello", 72.0, 0.0));
	assert!(large.width > small.width * 1.5);
	assert!(large.height > small.height * 1.5);
}

#[test]
fn cjk_text_measures_nonzero() {
	// No explicit family: the default fallback chain has to resolve the
	// glyphs through fontconfig.
	let size = measure(&plain("中文字体测试", 72.0, 0.0));
	assert!(size.width > 0.0, "width = {}", size.width);
	assert!(size.height > 0.0, "height = {}", size.height);
}

#[test]
fn empty_text_measures_zero() {
	let size = measure(&plain("", 72.0, 0.0));
	assert_eq!(size.width, 0.0);
	assert_eq!(size.height, 0.0);
}

#[test]
fn multi_line_is_taller_than_single_line() {
	let one = measure(&plain("Hello", 72.0, 0.0));
	let two = measure(&plain("Hello\nWorld", 72.0, 0.0));
	assert!(two.height > one.height);
}

#[test]
fn wrap_width_wraps() {
	let unwrapped = measure(&plain("中文字体测试", 72.0, 0.0));
	let wrapped = measure(&plain("中文字体测试", 72.0, 200.0));
	assert!(
		wrapped.width < unwrapped.width,
		"wrapped {} >= unwrapped {}",
		wrapped.width,
		unwrapped.width
	);
	assert!(wrapped.width <= 200.0, "wrapped width {}", wrapped.width);
	assert!(wrapped.height > unwrapped.height);
}

#[test]
fn html_mode_flattens_tags() {
	let plain_req = plain("Hello", 72.0, 0.0);
	let html_req = TextLayoutRequest {
		text: "<b>Hello</b>".to_string(),
		mode: TextLayoutMode::Html,
		..plain("Hello", 72.0, 0.0)
	};
	assert_eq!(measure(&html_req).width, measure(&plain_req).width);
	assert_eq!(measure(&html_req).height, measure(&plain_req).height);
}

#[test]
fn html_to_plain_decodes_and_breaks() {
	assert_eq!(html_to_plain("<b>a</b>"), "a");
	assert_eq!(html_to_plain("a<br>b"), "a\nb");
	assert_eq!(html_to_plain("a<br/>b"), "a\nb");
	assert_eq!(html_to_plain("<p>a</p><p>b</p>"), "a\nb");
	assert_eq!(html_to_plain("a</DIV>b"), "a\nb");
	assert_eq!(html_to_plain("&amp;&lt;&gt;&quot;&#39;&nbsp;"), "&<>\"' ");
	// Unknown entities and bare `&` survive untouched.
	assert_eq!(html_to_plain("a &b"), "a &b");
	// A decoded `&lt;p&gt;` is text, never rescanned into a tag.
	assert_eq!(html_to_plain("&lt;p&gt;"), "<p>");
	// Unterminated tags drop the remainder; trailing breaks are dropped.
	assert_eq!(html_to_plain("<p>a"), "a");
	assert_eq!(html_to_plain("a</p>"), "a");
}

#[test]
fn font_families_are_sorted_and_unique() {
	let _guard = FONT_DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let families = font_families();
	assert!(
		!families.is_empty(),
		"the system font database has no family"
	);
	assert_sorted_and_unique(&families);
	// Cached: the second call hands out the same list.
	assert_eq!(font_families(), families);
	// Clearing the cache only forces a rescan: the list is unchanged.
	clear_font_families_cache();
	assert_eq!(font_families(), families);
}

#[test]
fn add_font_data_rejects_empty() {
	assert!(!add_font_data(Vec::new()));
}

#[test]
fn add_font_dir_rejects_bogus_paths() {
	let _guard = FONT_DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	assert!(!add_font_dir(Path::new("/nonexistent/quercus-fonts")));
	// A path that does exist, but is a file rather than a directory.
	let dir = TempDir::new("bogus");
	let file = dir.path().join("font.ttf");
	std::fs::write(&file, b"not a font").expect("write the fake font");
	assert!(!add_font_dir(&file));
	assert!(!add_font_dir(&dir.path().join("missing")));
}

#[test]
fn add_font_data_refreshes_families() {
	let _guard = FONT_DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some((path, bytes)) = system_font() else {
		eprintln!("skipping add_font_data_refreshes_families: no system font found");
		return;
	};
	let before = font_families();
	assert!(!before.is_empty(), "the system font database has no family");
	let faces_before = face_count();

	// The list comes out of the cache until an add drops it.
	assert_eq!(font_families(), before);
	assert!(
		add_font_data(bytes),
		"adding {} was rejected",
		path.display()
	);

	// The add invalidated the cache, so the next list has to be rescanned.
	assert!(
		lock_families().is_none(),
		"the add did not invalidate the family cache"
	);

	// fontdb silently ignores malformed data, so a grown face count is
	// what proves these bytes were really parsed as a font.
	assert!(
		face_count() > faces_before,
		"adding {} added no face",
		path.display()
	);
	let after = font_families();
	assert!(!after.is_empty());
	assert!(
		after.len() >= before.len(),
		"the family list shrank from {} to {}",
		before.len(),
		after.len()
	);
	assert_sorted_and_unique(&after);
}

#[test]
fn add_font_dir_refreshes_families() {
	let _guard = FONT_DB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let Some((source, bytes)) = system_font() else {
		eprintln!("skipping add_font_dir_refreshes_families: no system font found");
		return;
	};
	let dir = TempDir::new("dir");
	let name = source
		.file_name()
		.expect("the scanned font has a file name");
	std::fs::write(dir.path().join(name), bytes).expect("copy the system font");
	// The cache is warm before the add, like it would be after the first
	// inspector build in the app.
	let before = font_families();
	let faces_before = face_count();

	assert!(
		add_font_dir(dir.path()),
		"adding {} was rejected",
		dir.path().display()
	);
	assert!(
		lock_families().is_none(),
		"the add did not invalidate the family cache"
	);
	assert!(
		face_count() > faces_before,
		"adding {} added no face",
		dir.path().display()
	);
	let after = font_families();
	assert!(!after.is_empty());
	assert!(after.len() >= before.len());
	assert_sorted_and_unique(&after);
}

#[test]
fn font_pixel_size_fallbacks() {
	let base = plain("x", 72.0, 0.0);
	// 72 pt at 3780 dots/meter == 96 DPI == 96.012 px.
	assert!((font_px(&base) as f64 - 96.012).abs() < 0.01);
	// 0 / non-finite point sizes fall back to 72 pt.
	assert_eq!(font_px(&plain("x", 0.0, 0.0)), font_px(&base));
	assert_eq!(font_px(&plain("x", f64::NAN, 0.0)), font_px(&base));
	// Non-positive dots-per-meter falls back to 3780.
	let mut req = plain("x", 72.0, 0.0);
	req.dots_per_meter = 0;
	assert_eq!(font_px(&req), font_px(&base));
	req.dots_per_meter = -3780;
	assert_eq!(font_px(&req), font_px(&base));
	// Absurd sizes clamp instead of asking swash for a huge bitmap.
	assert_eq!(font_px(&plain("x", 1.0e9, 0.0)), MAX_FONT_PX);
}

#[test]
fn render_writes_premultiplied_white_rgba() {
	let data = render_into(&plain("H", 72.0, 0.0), &scaled(1.0), 160, 160, 4);
	let mut ink = 0usize;
	for px in data.chunks_exact(4) {
		let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
		if a == 0 && r == 0 && g == 0 && b == 0 {
			continue;
		}
		ink += 1;
		assert_eq!((r, g, b), (r, r, r), "non-white pixel {px:?}");
		assert!(r <= a, "not premultiplied: {px:?}");
	}
	assert!(ink > 0, "the render painted nothing");
}

#[test]
fn render_writes_grayscale_coverage() {
	let req = plain("H", 72.0, 0.0);
	let gray = render_into(&req, &scaled(1.0), 160, 160, 1);
	assert!(has_ink(&gray), "the render painted nothing");
	// The RGBA alpha is that same coverage (both blend over a zeroed
	// buffer), so the two formats have to agree pixel for pixel.
	let rgba = render_into(&req, &scaled(1.0), 160, 160, 4);
	for (i, alpha) in rgba.chunks_exact(4).map(|px| px[3]).enumerate() {
		assert_eq!(gray[i], alpha, "coverage mismatch at pixel {i}");
	}
}

#[test]
fn render_scales_the_glyph() {
	let req = plain("H", 72.0, 0.0);
	let one = render_into(&req, &scaled(1.0), 256, 256, 1);
	let two = render_into(&req, &scaled(2.0), 256, 256, 1);
	let (x0, y0, x1, y1) = ink_bounds(&one, 256, 256).expect("scale 1 ink");
	let (u0, v0, u1, v1) = ink_bounds(&two, 256, 256).expect("scale 2 ink");
	assert!((u1 - u0) > (x1 - x0) * 3 / 2, "width did not scale");
	assert!((v1 - v0) > (y1 - y0) * 3 / 2, "height did not scale");
}

#[test]
fn render_skips_degenerate_scale() {
	for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
		let data = render_into(&plain("H", 72.0, 0.0), &scaled(scale), 64, 64, 1);
		assert!(!has_ink(&data), "scale {scale} painted something");
	}
}

#[test]
fn render_clips_to_the_clip_rect() {
	let req = plain("H", 72.0, 0.0);
	let full = render_into(&req, &scaled(1.0), 160, 160, 1);
	let (min_x, _, _, _) = ink_bounds(&full, 160, 160).expect("unclipped ink");
	assert!(min_x < 40, "the unclipped ink starts at {min_x}");

	// A clip that keeps only the right half of the glyph.
	let clipped = render_into(
		&req,
		&TextRenderTransform {
			clip_enabled: true,
			clip_offset_x: 40.0,
			clip_width: 200.0,
			clip_height: 200.0,
			..scaled(1.0)
		},
		160,
		160,
		1,
	);
	let (cmin_x, _, cmax_x, cmax_y) = ink_bounds(&clipped, 160, 160).expect("ink inside the clip");
	assert!(cmin_x >= 40, "ink left of the clip at {cmin_x}");
	assert!(cmin_x > min_x, "the clip removed nothing");
	assert!(cmax_x < 160 && cmax_y < 160);

	// An empty clip rect and a non-finite one paint nothing.
	let empty = render_into(
		&req,
		&TextRenderTransform {
			clip_enabled: true,
			clip_width: 0.0,
			clip_height: 0.0,
			..scaled(1.0)
		},
		160,
		160,
		1,
	);
	assert!(!has_ink(&empty));
	let nan = render_into(
		&req,
		&TextRenderTransform {
			clip_enabled: true,
			clip_offset_x: f64::NAN,
			clip_width: 64.0,
			clip_height: 64.0,
			..scaled(1.0)
		},
		160,
		160,
		1,
	);
	assert!(!has_ink(&nan));
}

#[test]
fn render_honors_draw_offset() {
	let req = plain("H", 72.0, 0.0);
	let base = render_into(&req, &scaled(1.0), 256, 256, 1);
	let shifted = render_into(
		&req,
		&TextRenderTransform {
			draw_offset_x: 100.0,
			draw_offset_y: 100.0,
			..scaled(1.0)
		},
		256,
		256,
		1,
	);
	let (bx, by, _, _) = ink_bounds(&base, 256, 256).expect("unshifted ink");
	let (sx, sy, _, _) = ink_bounds(&shifted, 256, 256).expect("shifted ink");
	// The offset is applied in device pixels after the scale, so the
	// corners move by exactly the offset.
	assert_eq!((sx, sy), (bx + 100, by + 100));
}

#[test]
fn hooks_install_and_drive_the_nodes() {
	let _guard = HOOK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	use quercus_node::nodes::textbackend::{text_measure_backend, text_render_backend};

	install();
	let measure_fn = text_measure_backend().expect("install() sets the measure hook");
	let render_fn = text_render_backend().expect("install() sets the render hook");

	let req = plain("Hello", 72.0, 0.0);
	let size = measure_fn(&req);
	assert!(size.width > 0.0 && size.height > 0.0);

	let mut data = vec![0u8; 192 * 192];
	render_fn(
		&req,
		&scaled(1.0),
		TextRenderTarget {
			data: &mut data,
			width: 192,
			height: 192,
			linesize_bytes: 192,
			channel_count: 1,
		},
	);
	assert!(has_ink(&data), "the render hook painted nothing");

	// Leave the process hooks uninstalled like `textbackend`'s own tests
	// do, so no later test depends on this module's state.
	set_text_backends(None, None);
}
