//! SRT subtitle import/export built on the text generator.
//!
//! Phase 1 keeps subtitle cues as ordinary Text clips on a video track: the
//! existing text renderer already previews and burns them in, and the
//! existing clip commands give undo/serialization for free. A dedicated
//! subtitle-block track is a later phase.
//!
//! Styling is applied to the Text clip's direct `text3` generator. The text
//! node only has inputs for font family/size, fill color, outline and
//! vertical alignment; the style's margins and line spacing have no matching
//! input and are carried for API compatibility only. The text node also has
//! no separate outline-enable input, so a style enables the outline whenever
//! [`SubtitleStyle::outline_width`] is greater than zero. `align` is defined
//! as horizontal left/center/right in the C ABI, but because text3 has no
//! horizontal alignment input it is mapped onto its vertical alignment combo
//! (0 = top, 1 = middle, 2 = bottom).

use std::path::Path;

use super::prelude::*;
use super::{
	generators::place_text_clip,
	project::{TEXT_FOOTAGE_TYPE_ID, create_text_footage_node, lock, node_type_id, track_behavior},
	push_multi_command,
	sequence::{clip_range, rational_to_ts, sequence_time_base, track_ids},
};
use quercus_codec::subtitles::{SubtitleCue, parse_srt, write_srt};
use quercus_node::block::clip_input;
use quercus_node::ops::set_value_at_time_command;
use quercus_node::value::NodeValue;

// The text3 input ids are not exported by `quercus_node::nodes::textv3`
// (the module is private), so the generator's stable public ids are repeated
// here. They are part of the serialized project format.
const FONT_FAMILY_INPUT: &str = "font_family_in";
const FONT_SIZE_INPUT: &str = "font_size_in";
const COLOR_INPUT: &str = "color_in";
const OUTLINE_ENABLED_INPUT: &str = "outline_enabled_in";
const OUTLINE_COLOR_INPUT: &str = "outline_color_in";
const OUTLINE_WIDTH_INPUT: &str = "outline_width_in";
const VERTICAL_ALIGNMENT_INPUT: &str = "valign_in";

/// Style applied to subtitle Text clips.
///
/// Values mirror the C ABI `QuercusSubtitleStyle`. The [`Default`] values
/// match the text3 node defaults: no font-family override, 72 pt, opaque
/// white fill, an enabled black 2 px outline.
#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleStyle {
	/// Font family name; empty selects the backend default font.
	pub font_family: String,
	/// Font size in points.
	pub font_size: f32,
	/// RGBA fill color.
	pub color: [f64; 4],
	/// RGBA outline color.
	pub outline_color: [f64; 4],
	/// Whether the outline pass is enabled. Width `0` still draws nothing.
	pub outline_enabled: bool,
	/// Outline width in points.
	pub outline_width: f32,
	/// Alignment selector (0, 1, 2). Mapped to text3 vertical alignment
	/// because the node has no horizontal alignment input.
	pub align: i32,
	/// Horizontal margin (no matching text3 input; ignored).
	pub margin_h: f32,
	/// Vertical margin (no matching text3 input; ignored).
	pub margin_v: f32,
	/// Line spacing multiplier (no matching text3 input; ignored).
	pub line_spacing: f32,
}

impl Default for SubtitleStyle {
	fn default() -> Self {
		Self {
			font_family: String::new(),
			font_size: 72.0,
			color: [1.0, 1.0, 1.0, 1.0],
			outline_color: [0.0, 0.0, 0.0, 1.0],
			outline_enabled: true,
			outline_width: 2.0,
			align: 1,
			margin_h: 0.0,
			margin_v: 0.0,
			line_spacing: 1.0,
		}
	}
}

impl SubtitleStyle {
	/// Validates the values that [`apply_subtitle_style`] writes to text3.
	///
	/// Font size must be finite and positive, outline width must be finite and
	/// non-negative, every color channel must be finite, and `align` must be
	/// one of the documented 0/1/2 selectors. The ignored margin and line
	/// spacing values are still required to be finite so a bad style is never
	/// silently accepted.
	pub fn validate(&self) -> Result<(), String> {
		if !self.font_size.is_finite() || self.font_size <= 0.0 {
			return Err(format!(
				"subtitle style: font size must be finite and greater than zero (got {})",
				self.font_size
			));
		}
		if !self.outline_width.is_finite() || self.outline_width < 0.0 {
			return Err(format!(
				"subtitle style: outline width must be finite and non-negative (got {})",
				self.outline_width
			));
		}
		for (name, color) in [
			("color", &self.color),
			("outline color", &self.outline_color),
		] {
			if !color.iter().all(|channel| channel.is_finite()) {
				return Err(format!("subtitle style: {name} must have finite channels"));
			}
		}
		for (name, value) in [
			("margin_h", self.margin_h),
			("margin_v", self.margin_v),
			("line_spacing", self.line_spacing),
		] {
			if !value.is_finite() {
				return Err(format!("subtitle style: {name} must be finite"));
			}
		}
		if !(0..=2).contains(&self.align) {
			return Err(format!(
				"subtitle style: align must be 0, 1 or 2 (got {})",
				self.align
			));
		}
		Ok(())
	}
}

/// The style's `align` selector mapped to text3's vertical alignment combo
/// (C++ `k_v_align_top/middle/bottom` = 0/1/2).
fn vertical_alignment(align: i32) -> i64 {
	match align {
		0 => 0,
		2 => 2,
		_ => 1,
	}
}

/// Builds the one undoable multi command that writes every text3 style input.
///
/// The commands are assembled before any is pushed, so a missing node/input
/// or an unexpected generator type leaves the graph untouched. The caller
/// must have validated `style` already.
fn push_style_commands(p: &ProjectRef, clip: NodeId, style: &SubtitleStyle) -> Result<(), String> {
	let commands = {
		let guard = lock(p);
		let source = guard
			.graph
			.connected_output(clip, clip_input::TEXTURE_INPUT, -1)
			.ok_or_else(|| "subtitle style: the clip has no texture source".to_string())?;
		let type_id = node_type_id(&guard.graph, source);
		if type_id != TEXT_FOOTAGE_TYPE_ID {
			return Err(format!(
				"subtitle style: the clip's texture source is not a text generator (found \"{type_id}\")"
			));
		}
		// `set_value_at_time_command` only reads the graph and captures the
		// project; holding the lock while building every command keeps the
		// aggregate consistent. The commands are pushed after the lock drops.
		let time = Rational::new(0, 1);
		let mut commands = Vec::with_capacity(7);
		let values = [
			(
				FONT_FAMILY_INPUT,
				NodeValue::StrCombo(style.font_family.clone()),
			),
			(FONT_SIZE_INPUT, NodeValue::Float(style.font_size as f64)),
			(COLOR_INPUT, NodeValue::Color(style.color)),
			(
				OUTLINE_ENABLED_INPUT,
				NodeValue::Boolean(style.outline_enabled),
			),
			(OUTLINE_COLOR_INPUT, NodeValue::Color(style.outline_color)),
			(
				OUTLINE_WIDTH_INPUT,
				NodeValue::Float(style.outline_width as f64),
			),
			(
				VERTICAL_ALIGNMENT_INPUT,
				NodeValue::Combo(vertical_alignment(style.align)),
			),
		];
		for (input, value) in values {
			commands.push(
				set_value_at_time_command(p, &guard.graph, source, input, -1, time, &value)
					.map_err(|e| format!("subtitle style: cannot set \"{input}\": {e}"))?,
			);
		}
		commands
	};
	push_multi_command(commands, "Subtitle Style")
}

/// Applies `style` to one subtitle Text clip as ONE undo row labelled
/// "Subtitle Style".
///
/// The clip must be a clip whose direct texture source is the text3
/// generator (`org.olivevideoeditor.Olive.text3`); anything else is
/// rejected. If an outer undo group is already open (for example during a
/// styled import) the multi command joins that group instead of creating a
/// second row.
pub fn apply_subtitle_style(
	project: &ProjectRef,
	clip: NodeId,
	style: &SubtitleStyle,
) -> Result<(), String> {
	style.validate()?;
	push_style_commands(project, clip, style)
}

/// Import every SRT cue as a Text clip on `track_index` with the default
/// subtitle style (one undo group). Returns the number of cues imported.
pub fn import_srt(
	project: &ProjectRef,
	sequence: NodeId,
	track_index: usize,
	path: &Path,
) -> Result<usize, String> {
	import_srt_styled(
		project,
		sequence,
		track_index,
		path,
		&SubtitleStyle::default(),
	)
}

/// Import every SRT cue as a Text clip on `track_index` and apply `style` to
/// each created clip. The whole import is one undo group, so the cue clips
/// and their style edits undo together. Returns the number of cues imported.
///
/// `style` is validated before anything is created; invalid values leave the
/// project and undo stack untouched.
pub fn import_srt_styled(
	project: &ProjectRef,
	sequence: NodeId,
	track_index: usize,
	path: &Path,
	style: &SubtitleStyle,
) -> Result<usize, String> {
	style.validate()?;
	let bytes =
		std::fs::read(path).map_err(|e| format!("failed to read \"{}\": {e}", path.display()))?;
	let cues = parse_srt(&bytes)?;
	if cues.is_empty() {
		return Ok(0);
	}
	let time_base = {
		let guard = lock(project);
		sequence_time_base(&guard.graph, sequence)
			.ok_or_else(|| "the sequence has no valid frame rate".to_string())?
	};
	quercus_undo::global::group_begin("Import Subtitles").map_err(|e| e.to_string())?;
	let result = (|| -> Result<usize, String> {
		for cue in &cues {
			let in_ts = rational_to_ts(Rational::from_double(cue.in_seconds), time_base).max(0);
			let out_ts =
				rational_to_ts(Rational::from_double(cue.out_seconds), time_base).max(in_ts + 1);
			let text = create_text_footage_node(project)?;
			// The text node's visible input is plain text; set it as one
			// undoable parameter edit inside the import group.
			super::parameters::set_text_value(project, text, "plain_text_in", -1, &cue.text)?;
			let clip = place_text_clip(project, sequence, text, track_index, in_ts, out_ts)?;
			// Joins the open "Import Subtitles" group as a single child.
			push_style_commands(project, clip, style)?;
		}
		Ok(cues.len())
	})();
	match &result {
		Ok(_) => quercus_undo::global::group_end().map_err(|e| e.to_string())?,
		Err(_) => {
			let _ = quercus_undo::global::group_abort();
		}
	}
	result
}

/// The display text stored on `node`, used when exporting subtitles.
///
/// `graph` is the project graph and `node` the text source to read. Looks at
/// the standard inputs `plain_text_in` and then `text_in` (element `-1`),
/// returning the first non-empty `NodeValue::Text`; returns `None` when the
/// node does not exist or neither input holds non-empty text. Empty strings
/// are skipped rather than returned, so a cue with cleared text is not
/// exported.
fn text_value(graph: &Graph, node: NodeId) -> Option<String> {
	let entry = graph.get(node)?;
	for input in ["plain_text_in", "text_in"] {
		if let quercus_node::value::NodeValue::Text(text) = &entry.core.standard_value(input, -1) {
			if !text.is_empty() {
				return Some(text.clone());
			}
		}
	}
	None
}

/// Export every Text clip on the sequence's video tracks to an SRT file.
/// Returns the number of cues written.
///
/// `project` is the project graph (locked while collecting), `sequence` the
/// sequence whose video tracks are scanned and `path` the SRT file to write
/// (created or overwritten). For each video-track clip the exporter takes its
/// in/out points (converted to seconds) and the text of the node connected to
/// the clip's `TEXTURE_INPUT` via [`text_value`]; clips without a range or a
/// text source are skipped silently. Cues are sorted by start time and then
/// by end time, then written by `write_srt`. Returns the cue count (0 produces
/// an empty file).
///
/// # Errors
///
/// Returns a `String` describing the failure when the file cannot be written
/// (including the path and the OS error).
pub fn export_srt(project: &ProjectRef, sequence: NodeId, path: &Path) -> Result<usize, String> {
	let mut cues = Vec::new();
	{
		let guard = lock(project);
		for track in track_ids(&guard.graph, sequence, TrackType::Video) {
			let Some(track_behavior) = track_behavior(&guard.graph, track) else {
				continue;
			};
			for block in &track_behavior.blocks {
				let Some((in_r, out_r, _)) = clip_range(&guard.graph, *block) else {
					continue;
				};
				let Some(source) =
					guard
						.graph
						.connected_output(*block, clip_input::TEXTURE_INPUT, -1)
				else {
					continue;
				};
				let Some(text) = text_value(&guard.graph, source) else {
					continue;
				};
				cues.push(SubtitleCue {
					in_seconds: in_r.to_f64(),
					out_seconds: out_r.to_f64(),
					text,
				});
			}
		}
	}
	cues.sort_by(|a, b| {
		a.in_seconds
			.total_cmp(&b.in_seconds)
			.then(a.out_seconds.total_cmp(&b.out_seconds))
	});
	let text = write_srt(&cues);
	std::fs::write(path, text)
		.map_err(|e| format!("failed to write \"{}\": {e}", path.display()))?;
	Ok(cues.len())
}

#[cfg(test)]
mod tests;

/// Reads the style currently applied to `clip`'s Text source.
///
/// `project` is the project graph (locked for the read) and `clip` the clip
/// block whose text styling should be recovered. Follows the clip's
/// `TEXTURE_INPUT` to its source node; returns `None` when the clip has no
/// texture input or the source is not a `text3` node (the text footprint the
/// subtitle importer creates), otherwise a [`SubtitleStyle`] with every field
/// read from the source's standard inputs: `font_family_in`
/// (StrCombo/Text), `font_size_in`, `color_in`, `outline_enabled_in`,
/// `outline_color_in`, `outline_width_in` and `valign_in`. A missing or
/// mistyped input keeps the [`SubtitleStyle::default`] value instead of
/// failing, so the result is a best-effort snapshot rather than a validation;
/// the vertical alignment lands in `align`.
pub fn read_subtitle_style(project: &ProjectRef, clip: NodeId) -> Option<SubtitleStyle> {
	let guard = lock(project);
	let source = guard
		.graph
		.connected_output(clip, clip_input::TEXTURE_INPUT, -1)?;
	if guard.graph.get(source)?.behavior.type_id() != "org.olivevideoeditor.Olive.text3" {
		return None;
	}
	let standard = |input: &str| {
		guard
			.graph
			.get(source)
			.map(|entry| entry.core.standard_value(input, -1))
	};
	let mut style = SubtitleStyle::default();
	if let Some(value) = standard("font_family_in") {
		style.font_family = match &value {
			NodeValue::StrCombo(text) | NodeValue::Text(text) => text.clone(),
			_ => style.font_family,
		};
	}
	if let Some(NodeValue::Float(size)) = standard("font_size_in") {
		style.font_size = size as f32;
	}
	if let Some(NodeValue::Color(color)) = standard("color_in") {
		style.color = color;
	}
	if let Some(NodeValue::Boolean(enabled)) = standard("outline_enabled_in") {
		style.outline_enabled = enabled;
	}
	if let Some(NodeValue::Color(color)) = standard("outline_color_in") {
		style.outline_color = color;
	}
	if let Some(NodeValue::Float(width)) = standard("outline_width_in") {
		style.outline_width = width as f32;
	}
	if let Some(NodeValue::Combo(align)) = standard("valign_in") {
		style.align = align as i32;
	}
	Some(style)
}
