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

//! cosmic-text backend for the text generator nodes: the facade-layer half
//! of [`quercus_node::nodes::textbackend`].
//!
//! `quercus-node` deliberately links no font or shaping crate: the text
//! generator nodes describe their job with the
//! [`TextLayoutRequest`](quercus_node::nodes::textbackend::TextLayoutRequest) /
//! [`TextRenderTarget`](quercus_node::nodes::textbackend::TextRenderTarget) PODs
//! and call the two function-pointer hooks that [`install`] fills in. This
//! module is that backend: layout and rasterization run on `cosmic-text` +
//! `swash`, the same stack the `gpui_wgpu` text system already links, so the
//! app keeps a single font stack and a single `cosmic-text` in the lockfile.
//!
//! [`RealEngine::new`](super::real::RealEngine::new) calls [`install`] once;
//! [`font_families`] feeds the `font_family_in` combo of the text nodes (see
//! [`super::effectchain::effect_params`]), and [`add_font_data`] /
//! [`add_font_dir`] let a caller register bundled fonts before or after
//! [`install`] without an app restart.
//!
//! # Units
//!
//! [`TextLayoutRequest::dots_per_meter`] is the paint device resolution
//! (Qt's `QTextDocument`/`QPainter` device metric); font sizes and geometry
//! in the request are points, and the laid-out document the hooks return and
//! draw in is sized in *device pixels*, exactly like the `QTextDocument`
//! this replaced. The conversion is `px = pt * dots_per_meter * 0.0254 / 72`
//! (3780 dots/meter, the Qt default for 96 DPI, gives the familiar 4/3
//! factor); a zero or negative `dots_per_meter` falls back to 3780 and a
//! non-positive or non-finite `font_size_pt` to 72 pt. Both fallbacks match
//! the text nodes' own defaults (`quercus_node::nodes::textv3`'s
//! `font_size_in` default is 72 pt).
//!
//! # Approximations compared to the former Qt implementation
//!
//! * Line spacing is `1.2 * font_size` (Qt's single spacing depends on the
//!   font's own metrics).
//! * `Html` / `OliveHtml` requests are flattened by [`html_to_plain`]
//!   instead of being laid out as rich text: tags carry no formatting here,
//!   and the text is painted white throughout — per-span colors, bold /
//!   italic runs and the rich-text alignment / list layout of the old
//!   `QTextDocument` are not reproduced.
//! * Text decorations (underline / strikethrough) are not painted.
//! * `center_horizontally` maps to [`Align::Center`] over the wrap width;
//!   like the C++ default `QTextOption(Qt::AlignCenter)` it is a no-op when
//!   the request carries no wrap width.
//! * A request whose flattened text is empty measures 0×0 and paints
//!   nothing (an empty `QTextDocument` reports one empty line's height).
//!
//! The layout and rasterization themselves follow the C++ contract to the
//! pixel: a document point `p` lands at
//! `((p.x + draw_offset_x) * scale, (p.y + draw_offset_y) * scale)`, clipped
//! to the scaled clip rect, and the two target formats are the Qt
//! `Format_Grayscale8` coverage buffer (channel count 1) and the
//! `Format_RGBA8888_Premultiplied` buffer (channel count 4).

use std::borrow::Cow;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, Once, OnceLock};

use cosmic_text::{
	Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap,
};
use quercus_node::nodes::textbackend::{
	set_text_backends, TextLayoutMode, TextLayoutRequest, TextLayoutSize, TextRenderTarget,
	TextRenderTransform,
};

/// Fallback font size in points (the text nodes' `font_size_in` default).
const DEFAULT_FONT_SIZE_PT: f64 = 72.0;

/// Fallback paint device resolution: 3780 dots/meter ≈ 96 DPI, Qt's default.
const DEFAULT_DOTS_PER_METER: f64 = 3780.0;

/// Fallback font size in device pixels, used only when the point size and
/// the resolution cannot produce a usable pixel size.
const DEFAULT_FONT_PX: f32 = 96.0;

/// Points per inch (the numerator of the point → pixel conversion).
const POINTS_PER_INCH: f64 = 72.0;

/// Meters per inch (the denominator of the point → pixel conversion).
const METERS_PER_INCH: f64 = 0.0254;

/// Line height as a multiple of the font size.
const LINE_HEIGHT_SCALE: f32 = 1.2;

/// Upper bound on the rasterized font size. A project file can carry an
/// arbitrary `font_size_pt` / `dots_per_meter`; without a ceiling the swash
/// bitmap cache would happily try to allocate gigabytes for a glyph.
const MAX_FONT_PX: f32 = 8192.0;

/// Upper bound on the wrap width, keeping the layout solver's inputs sane
/// for a corrupt `wrap_width`.
const MAX_WRAP_PX: f32 = 1.0e6;

/// Glyph rasterization state shared by every request.
///
/// [`FontSystem`] scans the system font directories on construction, so it
/// is built once (lazily, on the first non-empty request) and reused; a
/// [`Buffer`] is cheap and stays per-request.
struct TextSystem {
	font_system: FontSystem,
	swash_cache: SwashCache,
}

/// The process-wide text system.
fn system() -> &'static Mutex<TextSystem> {
	static SYSTEM: OnceLock<Mutex<TextSystem>> = OnceLock::new();
	SYSTEM.get_or_init(|| {
		Mutex::new(TextSystem {
			font_system: FontSystem::new(),
			swash_cache: SwashCache::new(),
		})
	})
}

/// Locks the text system, recovering from a poisoned lock: a panic while
/// shaping one text node must not take the whole app's text rendering down.
fn lock_system() -> MutexGuard<'static, TextSystem> {
	system().lock().unwrap_or_else(|e| e.into_inner())
}

/// Installs this module's hooks as the process-wide text backends (C++
/// `set_text_backends()` at app startup).
///
/// Idempotent: only the first call installs, so a second engine instance
/// cannot swap the hooks out from under a layout in flight.
pub fn install() {
	static INSTALL: Once = Once::new();
	INSTALL.call_once(|| {
		set_text_backends(Some(measure), Some(render));
	});
}

/// Measure hook: lays the request out and returns the document size in
/// device pixels (C++ `QTextDocument::size()`).
pub fn measure(req: &TextLayoutRequest) -> TextLayoutSize {
	let text = request_text(req);
	if text.is_empty() {
		return TextLayoutSize::default();
	}
	let mut sys = lock_system();
	let buffer = layout(&mut sys.font_system, req, &text);
	let mut width = 0.0f32;
	let mut height = 0.0f32;
	for run in buffer.layout_runs() {
		width = width.max(run.line_w);
		height = height.max(run.line_top + run.line_height);
	}
	TextLayoutSize {
		width: width as f64,
		height: height as f64,
	}
}

/// Render hook: paints the request into `target` (C++
/// `QAbstractTextDocumentLayout::draw()`).
///
/// Shapes the request, then rasterizes each glyph with swash and blends it
/// into the target: a document point `p` lands at
/// `((p.x + draw_offset_x) * scale, (p.y + draw_offset_y) * scale)`, clipped
/// to the scaled clip rect. Glyphs that cannot touch the clip rect are
/// skipped before rasterization so swash never caches them. Painting returns
/// early (nothing drawn) when the target has a non-positive size/linesize or
/// a channel count other than 1/4, when `scale` is non-finite or `<= 0`,
/// when the flattened text is empty, or when the clip rect is empty.
/// Existing target content is blended over, never cleared.
///
/// # Arguments
///
/// * `req` - the layout request; `Html`/`OliveHtml` are flattened to plain
///   text first.
/// * `transform` - painter transform (uniform scale, draw offset, optional
///   clip), all in pre-scale document coordinates.
/// * `target` - destination buffer and geometry; `channel_count == 1` is
///   8-bit grayscale coverage, `4` is premultiplied RGBA.
pub fn render(req: &TextLayoutRequest, transform: &TextRenderTransform, target: TextRenderTarget) {
	let TextRenderTarget {
		data,
		width,
		height,
		linesize_bytes,
		channel_count,
	} = target;
	if width <= 0 || height <= 0 || linesize_bytes <= 0 || !matches!(channel_count, 1 | 4) {
		return;
	}
	// `QPainter::scale(0, 0)` (degenerate transform in the stored
	// parameters) paints nothing rather than collapsing to a matrix.
	let scale = transform.scale;
	if !scale.is_finite() || scale <= 0.0 {
		return;
	}
	let text = request_text(req);
	if text.is_empty() {
		return;
	}
	let (clip_left, clip_top, clip_right, clip_bottom) = clip_rect(transform, width, height, scale);
	if !(clip_left < clip_right && clip_top < clip_bottom) {
		return;
	}

	let mut sys = lock_system();
	let buffer = layout(&mut sys.font_system, req, &text);
	let TextSystem {
		font_system,
		swash_cache,
	} = &mut *sys;
	let offset_x = transform.draw_offset_x * scale;
	for run in buffer.layout_runs() {
		let offset_y = (run.line_y as f64 + transform.draw_offset_y) * scale;
		for glyph in run.glyphs {
			let physical = glyph.physical((offset_x as f32, offset_y as f32), scale as f32);
			// The raster extends about one em around the glyph origin in
			// both axes; skip the glyphs that cannot touch the clip rect
			// instead of letting swash rasterize (and cache) them.
			let em = glyph.font_size as f64 * scale;
			let gx = physical.x as f64;
			let gy = physical.y as f64;
			if gx + em < clip_left
				|| gx - em > clip_right
				|| gy + em < clip_top
				|| gy - em > clip_bottom
			{
				continue;
			}
			let base = glyph.color_opt.unwrap_or(WHITE);
			swash_cache.with_pixels(font_system, physical.cache_key, base, |px, py, color| {
				let x = physical.x + px;
				let y = physical.y + py;
				if (x as f64) < clip_left
					|| (x as f64) >= clip_right
					|| (y as f64) < clip_top
					|| (y as f64) >= clip_bottom
				{
					return;
				}
				blend(data, linesize_bytes, channel_count, x, y, color);
			});
		}
	}
}

/// The default text color: the backends always paint white unless the
/// markup overrides it (C++ `QPalette::Text` = `Qt::white`).
const WHITE: Color = Color::rgb(0xFF, 0xFF, 0xFF);

/// Adds a font file's bytes to the process-wide font database.
///
/// The font is usable for layout and appears in [`font_families`] as soon as
/// this returns, whether the text backend is already installed or not; the
/// cached family list is invalidated, so the next call rescans the database.
/// Empty data is rejected with `false`.
///
/// Malformed data cannot be reported: the underlying
/// `Database::load_font_data` is infallible and silently skips the faces it
/// cannot parse, so non-empty garbage returns `true` and adds nothing.
/// Callers that need the distinction have to validate the bytes upstream.
///
/// Only *explicit* family selection sees the addition: the default fallback
/// chain is resolved when the font system is built, so a newly added font
/// does not join the fallbacks for glyphs the chosen family lacks.
pub fn add_font_data(data: Vec<u8>) -> bool {
	if data.is_empty() {
		return false;
	}
	// The database lock is released before the family cache is touched; the
	// two locks are never held in the opposite order (see [`font_families`]),
	// so this cannot deadlock.
	{
		let mut sys = lock_system();
		sys.font_system.db_mut().load_font_data(data);
	}
	clear_font_families_cache();
	true
}

/// Adds every font file under `path` (scanned recursively) to the
/// process-wide font database.
///
/// Like [`add_font_data`], the addition is visible to layout and to
/// [`font_families`] on its next call. Returns `false` when `path` is
/// missing or is not a directory; an existing directory that holds no font,
/// or only malformed ones, still returns `true`, because the scan ignores
/// whatever it cannot parse.
pub fn add_font_dir(path: &Path) -> bool {
	if !path.is_dir() {
		return false;
	}
	{
		let mut sys = lock_system();
		sys.font_system.db_mut().load_fonts_dir(path);
	}
	clear_font_families_cache();
	true
}

/// Drops the cached [`font_families`] list so the next call rescans the font
/// database.
///
/// [`add_font_data`] and [`add_font_dir`] already do this; it is public for
/// callers that changed the database through some other means.
pub fn clear_font_families_cache() {
	*lock_families() = None;
}

/// The cached family list; `None` means "rescan on the next
/// [`font_families`] call".
static FAMILIES: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// Locks the family cache, recovering from a poisoned lock like
/// [`lock_system`].
fn lock_families() -> MutexGuard<'static, Option<Vec<String>>> {
	FAMILIES.lock().unwrap_or_else(|e| e.into_inner())
}

/// The sorted, de-duplicated font families of the system font database.
///
/// Feeds the text nodes' `font_family_in` combo (the `combo_option`
/// injection in [`super::effectchain::effect_params`]). Names are the
/// English family names, so they match what a project stores; an empty
/// database yields an empty list and the combo keeps free-form entry.
///
/// The list is cached because `effect_params` rebuilds the inspector's
/// parameters on every engine change; [`add_font_data`] / [`add_font_dir`]
/// invalidate it, so a caller that registers fonts after the first call
/// still sees them.
pub fn font_families() -> Vec<String> {
	// The cache lock is held while the database is scanned, so an add landing
	// in between cannot be lost: the add needs the (exclusive) system lock to
	// change the database, then clears this cache afterwards, which blocks
	// until this snapshot is stored and drops it again.
	let mut cache = lock_families();
	if let Some(families) = cache.as_ref() {
		return families.clone();
	}
	let mut names: Vec<String> = {
		let sys = lock_system();
		sys.font_system
			.db()
			.faces()
			.filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
			.collect()
	};
	names.sort();
	names.dedup();
	*cache = Some(names.clone());
	names
}

/// The pixel-space clip rectangle of a render, already intersected with the
/// target buffer. An empty (or inverted) rectangle means nothing is drawn;
/// a non-finite rect from a corrupt transform collapses to empty too.
fn clip_rect(
	transform: &TextRenderTransform,
	width: i32,
	height: i32,
	scale: f64,
) -> (f64, f64, f64, f64) {
	let mut left = 0.0f64;
	let mut top = 0.0f64;
	let mut right = width as f64;
	let mut bottom = height as f64;
	if transform.clip_enabled {
		let x = transform.clip_offset_x * scale;
		let y = transform.clip_offset_y * scale;
		let w = transform.clip_width * scale;
		let h = transform.clip_height * scale;
		if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite()) {
			return (0.0, 0.0, 0.0, 0.0);
		}
		left = left.max(x);
		top = top.max(y);
		right = right.min(x + w.max(0.0));
		bottom = bottom.min(y + h.max(0.0));
	}
	(left, top, right, bottom)
}

/// Composites one source pixel over the target.
///
/// `channel_count == 1` is the grayscale coverage buffer the v1/v2 nodes
/// tint afterwards: the glyph contributes its alpha as coverage. Channel
/// count 4 is the premultiplied RGBA buffer of v3, so the source is
/// premultiplied before the over-blend (the hook's colors are straight —
/// swash's mask pixels carry the (white) base color plus coverage-as-alpha,
/// and its color bitmaps are straight RGBA).
fn blend(data: &mut [u8], linesize_bytes: i32, channel_count: i32, x: i32, y: i32, color: Color) {
	if x < 0 || y < 0 {
		return;
	}
	let alpha = color.a();
	if alpha == 0 {
		return;
	}
	let offset = y as usize * linesize_bytes as usize + x as usize * channel_count as usize;
	if channel_count == 1 {
		let Some(dst) = data.get_mut(offset) else {
			return;
		};
		let src = alpha as u32;
		let out = src + (*dst as u32 * (255 - src)) / 255;
		*dst = out.min(255) as u8;
		return;
	}
	let Some(pixel) = data.get_mut(offset..offset + 4) else {
		return;
	};
	let src_alpha = alpha as u32;
	let inverse = 255 - src_alpha;
	let src = [
		(color.r() as u32 * src_alpha + 127) / 255,
		(color.g() as u32 * src_alpha + 127) / 255,
		(color.b() as u32 * src_alpha + 127) / 255,
		src_alpha,
	];
	for (dst, value) in pixel.iter_mut().zip(src) {
		*dst = (value + *dst as u32 * inverse / 255).min(255) as u8;
	}
}

/// The text a request lays out: the request's own text, or the flattened
/// markup for the two HTML modes.
fn request_text(req: &TextLayoutRequest) -> Cow<'_, str> {
	match req.mode {
		TextLayoutMode::PlainText => Cow::Borrowed(req.text.as_str()),
		TextLayoutMode::Html | TextLayoutMode::OliveHtml => Cow::Owned(html_to_plain(&req.text)),
	}
}

/// Lays a request out into a shaped [`Buffer`].
///
/// The buffer uses `LINE_HEIGHT_SCALE * font_px` line spacing, the request's
/// family when non-empty, centered alignment when `center_horizontally` is
/// set, and `Wrap::WordOrGlyph` so CJK text also breaks inside a run. The
/// wrap width is applied only when finite and positive, clamped to
/// `MAX_WRAP_PX`. The caller passes the already-flattened text and owns the
/// font-system lock.
///
/// # Arguments
///
/// * `font_system` - shaping backend; the returned buffer belongs to it.
/// * `req` - the layout request (family, point size, dots-per-meter, wrap
///   width, horizontal centering).
/// * `text` - flattened text to shape (`request_text(req)`).
fn layout(font_system: &mut FontSystem, req: &TextLayoutRequest, text: &str) -> Buffer {
	let metrics = Metrics::relative(font_px(req), LINE_HEIGHT_SCALE);
	let mut buffer = Buffer::new(font_system, metrics);
	let mut attrs = Attrs::new();
	if !req.font_family.is_empty() {
		attrs = attrs.family(Family::Name(req.font_family.as_str()));
	}
	let alignment = if req.center_horizontally {
		Some(Align::Center)
	} else {
		None
	};
	buffer.set_text(text, &attrs, Shaping::Advanced, alignment);
	if req.wrap_width.is_finite() && req.wrap_width > 0.0 {
		buffer.set_size(Some(req.wrap_width.min(MAX_WRAP_PX as f64) as f32), None);
	}
	// CJK text has no spaces to break at, so word wrapping alone would
	// overflow the wrap width; `WordOrGlyph` keeps the C++ behavior of
	// wrapping inside a run of CJK.
	buffer.set_wrap(Wrap::WordOrGlyph);
	buffer.shape_until_scroll(font_system, false);
	buffer
}

/// The font size of a request in device pixels, with the documented
/// fallbacks and a sanity ceiling.
fn font_px(req: &TextLayoutRequest) -> f32 {
	let pt = if req.font_size_pt.is_finite() && req.font_size_pt > 0.0 {
		req.font_size_pt
	} else {
		DEFAULT_FONT_SIZE_PT
	};
	let dots_per_meter = if req.dots_per_meter > 0 {
		req.dots_per_meter as f64
	} else {
		DEFAULT_DOTS_PER_METER
	};
	let px = pt * dots_per_meter * (METERS_PER_INCH / POINTS_PER_INCH);
	if !px.is_finite() || px <= 0.0 {
		return DEFAULT_FONT_PX;
	}
	(px as f32).min(MAX_FONT_PX)
}

/// Flattens the HTML the text nodes may carry into plain text.
///
/// This mirrors `quercus_node::nodes::textv3`'s legacy-HTML stripper (which
/// is crate-private): tags are dropped without being rescanned, the
/// entities it knows are decoded, unknown entities and bare `&` are kept
/// as-is, and whitespace is neither collapsed nor trimmed. On top of that,
/// `<br>` and the block-level end tags (`</p>`, `</div>`, `</li>`, the
/// headings, table rows, …) become line breaks so the paragraph structure
/// of `Html` / `OliveHtml` text survives flattening; the breaks a trailing
/// block end tag would add are dropped.
fn html_to_plain(html: &str) -> String {
	/// Whether `chars` starts with the (ASCII) `entity` text.
	///
	/// Compares as many leading characters as the entity has, so an entity longer
	/// than `chars` yields `false`. Only the literal character sequence is
	/// matched: no case folding, trimming or entity normalization happens.
	///
	/// # Arguments
	///
	/// * `chars` - remaining input as `char`s (the caller's cursor position).
	/// * `entity` - spelling to match, e.g. `"&amp;"`.
	fn starts_with(chars: &[char], entity: &str) -> bool {
		let mut it = chars.iter();
		entity.chars().all(|c| it.next() == Some(&c))
	}

	const ENTITIES: [(&str, &str); 6] = [
		("&amp;", "&"),
		("&lt;", "<"),
		("&gt;", ">"),
		("&quot;", "\""),
		("&#39;", "'"),
		("&nbsp;", " "),
	];

	/// Block-level end tags that start a new line, plus `<br>` itself.
	const LINE_BREAK_TAGS: [&str; 21] = [
		"br",
		"/p",
		"/div",
		"/li",
		"/tr",
		"/h1",
		"/h2",
		"/h3",
		"/h4",
		"/h5",
		"/h6",
		"/blockquote",
		"/pre",
		"/table",
		"/ul",
		"/ol",
		"/dl",
		"/dt",
		"/dd",
		"/section",
		"/figure",
	];

	let chars: Vec<char> = html.chars().collect();
	let mut out = String::with_capacity(html.len());
	let mut i = 0;
	while i < chars.len() {
		match chars[i] {
			'<' => {
				// Drop up to and including the tag's closing '>'; an
				// unterminated tag drops the remainder. The tag text is
				// discarded, never rescanned, so a decoded `&lt;p&gt;`
				// cannot turn into a tag afterwards.
				let start = i + 1;
				let mut end = start;
				while end < chars.len() && chars[end] != '>' {
					end += 1;
				}
				let tag: String = chars[start..end].iter().collect::<String>().to_lowercase();
				let trimmed = tag.trim_start();
				let (closing, rest) = match trimmed.strip_prefix('/') {
					Some(rest) => (true, rest.trim_start()),
					None => (false, trimmed),
				};
				let name: String = rest
					.chars()
					.take_while(|c| c.is_ascii_alphanumeric())
					.collect();
				let name = if closing { format!("/{name}") } else { name };
				if !out.is_empty() && LINE_BREAK_TAGS.contains(&name.as_str()) {
					out.push('\n');
				}
				i = end + 1;
			}
			'&' => {
				let decoded = ENTITIES
					.iter()
					.find(|(entity, _)| starts_with(&chars[i..], entity));
				match decoded {
					Some((entity, replacement)) => {
						out.push_str(replacement);
						i += entity.chars().count();
					}
					None => {
						// Unknown entity (or a bare '&'): keep it as-is.
						out.push('&');
						i += 1;
					}
				}
			}
			c => {
				out.push(c);
				i += 1;
			}
		}
	}
	while out.ends_with('\n') {
		out.pop();
	}
	out
}

#[cfg(test)]
mod tests;
