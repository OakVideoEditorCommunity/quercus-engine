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

//! Tracks and track lists (C++ `Track`, `TrackList`).
//! `// CPP-PARITY: src/node/src/output/track/track.{h,cpp}`.

use crate::id::NodeId;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Track media type (values match C++ `Track::Type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackType {
	/// Video.
	Video,
	/// Audio.
	Audio,
	/// Subtitle.
	Subtitle,
}

impl TrackType {
	/// C ABI value (`QuercusNodeTrackType`: NONE=-1, VIDEO=0, AUDIO=1,
	/// SUBTITLE=2).
	pub fn to_c(self) -> i32 {
		match self {
			TrackType::Video => 0,
			TrackType::Audio => 1,
			TrackType::Subtitle => 2,
		}
	}

	/// From a C ABI value; `None` for anything outside 0..=2.
	///
	/// `v` is a `QuercusNodeTrackType` integer (VIDEO=0, AUDIO=1,
	/// SUBTITLE=2); `-1` (NONE) and any other value yield `None`. Inverse of
	/// [`TrackType::to_c`].
	pub fn from_c(v: i32) -> Option<TrackType> {
		match v {
			0 => Some(TrackType::Video),
			1 => Some(TrackType::Audio),
			2 => Some(TrackType::Subtitle),
			_ => None,
		}
	}
}

/// Track behavior: an ordered block list (C++ `Track`).
#[derive(Clone)]
pub struct TrackBehavior {
	/// Media type.
	pub kind: TrackType,
	/// Block node ids in timeline order.
	pub blocks: Vec<NodeId>,
	/// Muted flag.
	pub muted: bool,
	/// Locked flag.
	pub locked: bool,
	/// Height in internal units (C++ `track_height_`).
	pub height: f64,
	/// Index inside its track list (C++ `index_`).
	pub index: i32,
	/// Owning track list id (None when detached).
	pub track_list: Option<NodeId>,
}

/// Track list behavior (C++ `TrackList`): the per-type collection of
/// tracks inside a sequence.
#[derive(Clone)]
pub struct TrackListBehavior {
	/// Media type of this list.
	pub kind: TrackType,
	/// Track node ids in stack order.
	pub tracks: Vec<NodeId>,
	/// Owning sequence id (None when detached).
	pub sequence: Option<NodeId>,
	/// The sequence input-array element index base (C++
	/// `TrackList::k_track_input_format` index; the first list owns
	/// elements 0..n, the second n.., etc.).
	pub array_base: i32,
}

/// Default track height in internal units (C++ `k_track_height_default`).
pub const DEFAULT_HEIGHT_INTERNAL: f64 = 3.0;
/// Minimum track height in internal units (C++ `k_track_height_minimum`).
pub const MINIMUM_HEIGHT_INTERNAL: f64 = 1.5;
/// Default font height in pixels (C++ `Track::default_font_height`).
pub const DEFAULT_FONT_HEIGHT: f64 = 13.0;

/// Internal -> pixel height (C++ `Track::internal_height_to_pixel_height`).
///
/// `h` is a height in internal units (see [`DEFAULT_HEIGHT_INTERNAL`]); the
/// result is `h * DEFAULT_FONT_HEIGHT` (13.0) rounded to the nearest pixel and
/// returned as `i32`. Negative input is not clamped. Counterpart of
/// [`pixel_height_to_internal_height`].
pub fn internal_height_to_pixel_height(h: f64) -> i32 {
	(h * DEFAULT_FONT_HEIGHT).round() as i32
}

/// Pixel -> internal height (C++ `Track::pixel_height_to_internal_height`).
///
/// `h` is a pixel height; the result is `h / DEFAULT_FONT_HEIGHT` (13.0) with
/// no rounding. Inverse of [`internal_height_to_pixel_height`], which rounds —
/// so a pixel round-trip is exact but an internal round-trip lands on the
/// nearest 1/13th step.
pub fn pixel_height_to_internal_height(h: i32) -> f64 {
	h as f64 / DEFAULT_FONT_HEIGHT
}

impl TrackBehavior {
	/// New track of the given type.
	///
	/// `kind` sets the media type. The track starts empty: no blocks, not
	/// muted, not locked, [`DEFAULT_HEIGHT_INTERNAL`] high, index 0 and not
	/// attached to a track list (the owning list sets `index` and
	/// `track_list` when it inserts the track).
	pub fn new(kind: TrackType) -> Self {
		TrackBehavior {
			kind,
			blocks: Vec::new(),
			muted: false,
			locked: false,
			height: DEFAULT_HEIGHT_INTERNAL,
			index: 0,
			track_list: None,
		}
	}

	/// Constructor for the serializer: a track core carries only the
	/// inherited `enabled_in` (C++ `Track` also declares the `block_in`
	/// array and `arraymap_in`; the Rust model keeps block membership in
	/// [`TrackBehavior::blocks`]).
	pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
		(NodeCore::new(), Box::new(TrackBehavior::new(TrackType::Video)))
	}

	/// Block at `index` (None out of range).
	///
	/// `index` is a position in the track's ordering vector (0 = first block
	/// in timeline order); returns its node id, or `None` when the track has
	/// fewer blocks. Counterpart of [`Self::block_index`].
	pub fn block_at(&self, index: usize) -> Option<NodeId> {
		self.blocks.get(index).copied()
	}

	/// Index of `block` in the block list.
	///
	/// `block` is a block node id; the track's ordering vector is scanned
	/// linearly, so the result is its position in timeline order. Returns
	/// `None` when the block is not on this track.
	pub fn block_index(&self, block: NodeId) -> Option<usize> {
		self.blocks.iter().position(|b| *b == block)
	}

	/// Append a block (C++ `Track::append_block`).
	///
	/// `block` is pushed onto the end of `blocks` (the track's ordering
	/// vector); the block's own in/out times are not touched, so callers place
	/// it on the timeline separately. No duplicate check; counterpart of
	/// [`Self::prepend_block`] and [`Self::insert_block_after`].
	pub fn append_block(&mut self, block: NodeId) {
		self.blocks.push(block);
	}

	/// Prepend a block (C++ `Track::prepend_block`).
	///
	/// `block` becomes the first entry of `blocks`, shifting every existing
	/// block one position later. No duplicate check and no timeline re-layout;
	/// counterpart of [`Self::append_block`].
	pub fn prepend_block(&mut self, block: NodeId) {
		self.blocks.insert(0, block);
	}

	/// Insert a block at `index` (clamped; C++ `insert_block_at_index`).
	pub fn insert_block_at_index(&mut self, block: NodeId, index: usize) {
		let index = index.min(self.blocks.len());
		self.blocks.insert(index, block);
	}

	/// Insert `block` after `before` (C++ `insert_block_after`).
	///
	/// `block` is the id to insert and `before` is the existing block it
	/// should follow (the C++ argument names the anchor, not the new block).
	/// Returns `true` when `before` was found and `block` was inserted
	/// immediately after it, `false` (list unchanged) when `before` is not in
	/// this track. No duplicate check and no timeline re-layout; only the
	/// ordering vector changes.
	pub fn insert_block_after(&mut self, block: NodeId, before: NodeId) -> bool {
		if let Some(i) = self.block_index(before) {
			self.blocks.insert(i + 1, block);
			true
		} else {
			false
		}
	}

	/// Insert `block` before `after` (C++ `insert_block_before`).
	///
	/// `block` is the id to insert and `after` is the existing block it should
	/// precede (the parameter is named after the C++ argument, which names the
	/// anchor, not the new block). Returns `true` when `after` was found and
	/// `block` was inserted immediately before it, `false` (list unchanged)
	/// when `after` is not in this track. The new block is not checked for
	/// duplicates and no timeline positions are touched; only the ordering
	/// vector changes.
	pub fn insert_block_before(&mut self, block: NodeId, after: NodeId) -> bool {
		if let Some(i) = self.block_index(after) {
			self.blocks.insert(i, block);
			true
		} else {
			false
		}
	}

	/// Remove `block` (no-op when absent; C++ `Track::remove_block`).
	pub fn remove_block(&mut self, block: NodeId) -> bool {
		let before = self.blocks.len();
		self.blocks.retain(|b| *b != block);
		self.blocks.len() != before
	}

	/// Ripple-remove: drop the block and shift the successors' positions
	/// earlier by the block's length (C++ `Track::ripple_remove_block`).
	pub fn ripple_remove_block(&mut self, block: NodeId) -> bool {
		self.remove_block(block)
	}

	/// Replace `old` with `replace` (C++ `Track::replace_block`); both
	/// must have equal lengths (the caller validates).
	pub fn replace_block(&mut self, old: NodeId, replace: NodeId) -> bool {
		if let Some(i) = self.block_index(old) {
			self.blocks[i] = replace;
			true
		} else {
			false
		}
	}

	/// Block strictly containing `time` (in < time < out; C++
	/// `block_containing_time`).
	pub fn block_containing_time(
		&self,
		time: quercus_core::Rational,
		blocks: &dyn BlockRange,
	) -> Option<NodeId> {
		self.blocks
			.iter()
			.find(|b| blocks.contains_strict(**b, time))
			.copied()
	}

	/// Block visible at `time` (in <= time < out; C++
	/// `visible_block_at_time`).
	pub fn visible_block_at_time(
		&self,
		time: quercus_core::Rational,
		blocks: &dyn BlockRange,
	) -> Option<NodeId> {
		self.blocks
			.iter()
			.find(|b| blocks.contains(**b, time))
			.copied()
	}

	/// Whether the [in, out) range holds no block or only a gap (C++
	/// `is_range_free`).
	pub fn is_range_free(&self, range: quercus_core::TimeRange, blocks: &dyn BlockRange) -> bool {
		!self.blocks.iter().any(|b| blocks.overlaps(*b, range))
	}

	/// Total length (end of the last block; C++ `Track::get_length`).
	pub fn length(&self, blocks: &dyn BlockRange) -> quercus_core::Rational {
		let mut end = quercus_core::Rational::new(0, 1);
		for b in &self.blocks {
			let out = blocks.out(*b);
			if out > end {
				end = out;
			}
		}
		end
	}

	/// Track reference as (type, index) (C++ `Track::Reference`).
	pub fn reference(&self) -> (i32, i32) {
		(self.kind.to_c(), self.index)
	}
}

/// Block-range accessor trait: the graph-backed queries the track needs
/// (block in/out/length) without coupling track.rs to the graph arena.
pub trait BlockRange {
	/// In-point of `block`.
	fn in_(&self, block: NodeId) -> quercus_core::Rational;
	/// Out-point of `block`.
	fn out(&self, block: NodeId) -> quercus_core::Rational;
	/// True when `time` is strictly inside the block.
	///
	/// Unlike [`Self::contains`], both endpoints are excluded, so the in-point
	/// and out-point of the block do not count. Used when picking a block a
	/// cut belongs to (see [`TrackBehavior::block_containing_time`]), where a
	/// time sitting exactly on a boundary must not select either neighbour.
	fn contains_strict(&self, block: NodeId, time: quercus_core::Rational) -> bool {
		let (in_, out) = (self.in_(block), self.out(block));
		time > in_ && time < out
	}
	/// True when `time` is visible on the block (in <= t < out).
	///
	/// `time` is a timeline time and `block` a block known to the accessor;
	/// the span is half-open, so a time exactly on the in-point counts but one
	/// on the out-point (the first instant after the block) does not. This is
	/// the test behind [`TrackBehavior::visible_block_at_time`].
	fn contains(&self, block: NodeId, time: quercus_core::Rational) -> bool {
		let (in_, out) = (self.in_(block), self.out(block));
		time >= in_ && time < out
	}
	/// True when the block's span overlaps `range`.
	///
	/// Compares the block's half-open `[in, out)` span with `range` (also
	/// half-open): returns `false` when the block ends at or before the
	/// range's in-point, or starts at or after its out-point, so merely
	/// touching endpoints are not an overlap. `block` must be a block known to
	/// the accessor.
	fn overlaps(&self, block: NodeId, range: quercus_core::TimeRange) -> bool {
		let (in_, out) = (self.in_(block), self.out(block));
		!(out <= range.in_() || in_ >= range.out())
	}
}

impl TrackListBehavior {
	/// New empty list.
	///
	/// `kind` is the media type every track in the list must share; the list
	/// starts with no tracks, no owning sequence and `array_base == 0` (the
	/// caller re-bases it when the sequence owns several lists of one type).
	/// Used by [`Self::create`] for deserialization.
	pub fn new(kind: TrackType) -> Self {
		TrackListBehavior {
			kind,
			tracks: Vec::new(),
			sequence: None,
			array_base: 0,
		}
	}

	/// Constructor for the serializer: a track list core carries only
	/// the inherited `enabled_in` (the list's state lives in the
	/// behavior; the C++ `TrackList` is a plain QObject, not a node).
	pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
		(NodeCore::new(), Box::new(TrackListBehavior::new(TrackType::Video)))
	}

	/// Track at `index`.
	///
	/// `index` is the position in `tracks` (stack order, 0 at the bottom);
	/// returns the track's node id, or `None` when the index is out of range.
	/// Counterpart of [`Self::track_index`].
	pub fn track_at(&self, index: usize) -> Option<NodeId> {
		self.tracks.get(index).copied()
	}

	/// Index of `track` in the list.
	///
	/// `track` is the track node id to look up; the list is scanned in stack
	/// order, so the result is the track's position in `tracks` (and its
	/// timeline row). Returns `None` when the id is not part of this list.
	pub fn track_index(&self, track: NodeId) -> Option<usize> {
		self.tracks.iter().position(|t| *t == track)
	}

	/// Combined length of the longest track (C++
	/// `TrackList::get_total_length`).
	pub fn total_length(&self, tracks: &dyn TrackRange) -> quercus_core::Rational {
		let mut longest = quercus_core::Rational::new(0, 1);
		for t in &self.tracks {
			let len = tracks.length(*t);
			if len > longest {
				longest = len;
			}
		}
		longest
	}
}

impl NodeBehavior for TrackBehavior {
	fn name(&self) -> &str {
		match self.kind {
			TrackType::Video => "Video Track",
			TrackType::Audio => "Audio Track",
			TrackType::Subtitle => "Subtitle Track",
		}
	}

	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.track"
	}

	fn categories(&self) -> &[Category] {
		&[Category::Timeline]
	}

	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(TrackBehavior {
			kind: self.kind,
			blocks: self.blocks.clone(),
			muted: self.muted,
			locked: self.locked,
			height: self.height,
			index: self.index,
			track_list: self.track_list,
		}))
	}

	/// Custom project save (C++ `Track::SaveCustom` writes only the
	/// height; the type/muted/locked and the block/track-list
	/// references are Rust additions that older readers skip).
	fn save_custom(&self, core: &NodeCore, writer: &mut dyn crate::serializer::XmlWrite) {
		let _ = core;
		writer.text_element("type", &self.kind.to_c().to_string());
		writer.text_element("index", &self.index.to_string());
		writer.text_element("muted", if self.muted { "1" } else { "0" });
		writer.text_element("locked", if self.locked { "1" } else { "0" });
		writer.text_element("height", &format!("{}", self.height));
		if let Some(tl) = self.track_list {
			writer.text_element("tracklist", &tl.identity().to_string());
		}
		if !self.blocks.is_empty() {
			writer.start_element("blocks");
			for b in &self.blocks {
				writer.text_element("block", &b.identity().to_string());
			}
			writer.end_element(); // blocks
		}
	}

	/// Custom project load. The block/track-list references are packed
	/// ids resolved by the serializer's post-load pass; a track saved by
	/// C++ carries only `<height>`, so the kind is filled from the
	/// sequence `track_in_%1` connection when the custom has none.
	fn load_custom(
		&mut self,
		_core: &mut NodeCore,
		reader: &mut dyn crate::serializer::XmlRead,
	) -> bool {
		while reader.next_start_element() {
			match reader.name() {
				"type" => {
					if let Some(kind) = reader
						.read_element_text()
						.trim()
						.parse::<i32>()
						.ok()
						.and_then(TrackType::from_c)
					{
						self.kind = kind;
					}
				}
				"index" => {
					self.index = reader.read_element_text().trim().parse().unwrap_or(self.index)
				}
				"muted" => self.muted = reader.read_element_text().trim() == "1",
				"locked" => self.locked = reader.read_element_text().trim() == "1",
				"height" => {
					self.height = reader
						.read_element_text()
						.trim()
						.parse()
						.unwrap_or(self.height)
				}
				"tracklist" => {
					self.track_list = crate::serializer::parse_node_ref(&reader.read_element_text())
				}
				"blocks" => {
					self.blocks.clear();
					while reader.next_start_element() {
						if reader.name() == "block" {
							if let Some(id) =
								crate::serializer::parse_node_ref(&reader.read_element_text())
							{
								self.blocks.push(id);
							}
						} else {
							reader.skip_current_element();
						}
					}
				}
				_ => reader.skip_current_element(),
			}
		}
		true
	}

	fn as_any(&self) -> Option<&dyn std::any::Any> {
		Some(self)
	}

	fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
		Some(self)
	}
}

impl NodeBehavior for TrackListBehavior {
	fn name(&self) -> &str {
		match self.kind {
			TrackType::Video => "Video Tracks",
			TrackType::Audio => "Audio Tracks",
			TrackType::Subtitle => "Subtitle Tracks",
		}
	}

	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.tracklist"
	}

	fn categories(&self) -> &[Category] {
		&[Category::Timeline]
	}

	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(TrackListBehavior {
			kind: self.kind,
			tracks: self.tracks.clone(),
			sequence: self.sequence,
			array_base: self.array_base,
		}))
	}

	/// Custom project save: the list kind, array base and its node
	/// references (the C++ `TrackList` has no serialization of its own —
	/// it is a plain QObject owned by the sequence).
	fn save_custom(&self, core: &NodeCore, writer: &mut dyn crate::serializer::XmlWrite) {
		let _ = core;
		writer.text_element("type", &self.kind.to_c().to_string());
		writer.text_element("arraybase", &self.array_base.to_string());
		if let Some(s) = self.sequence {
			writer.text_element("sequence", &s.identity().to_string());
		}
		if !self.tracks.is_empty() {
			writer.start_element("tracks");
			for t in &self.tracks {
				writer.text_element("track", &t.identity().to_string());
			}
			writer.end_element(); // tracks
		}
	}

	/// Custom project load; references resolve in the serializer's
	/// post-load pass.
	fn load_custom(
		&mut self,
		_core: &mut NodeCore,
		reader: &mut dyn crate::serializer::XmlRead,
	) -> bool {
		while reader.next_start_element() {
			match reader.name() {
				"type" => {
					if let Some(kind) = reader
						.read_element_text()
						.trim()
						.parse::<i32>()
						.ok()
						.and_then(TrackType::from_c)
					{
						self.kind = kind;
					}
				}
				"arraybase" => {
					self.array_base = reader.read_element_text().trim().parse().unwrap_or(0)
				}
				"sequence" => {
					self.sequence = crate::serializer::parse_node_ref(&reader.read_element_text())
				}
				"tracks" => {
					self.tracks.clear();
					while reader.next_start_element() {
						if reader.name() == "track" {
							if let Some(id) =
								crate::serializer::parse_node_ref(&reader.read_element_text())
							{
								self.tracks.push(id);
							}
						} else {
							reader.skip_current_element();
						}
					}
				}
				_ => reader.skip_current_element(),
			}
		}
		true
	}

	fn as_any(&self) -> Option<&dyn std::any::Any> {
		Some(self)
	}

	fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
		Some(self)
	}
}

/// Track-length accessor trait (the graph-backed query the list needs).
pub trait TrackRange {
	/// Total length of `track`.
	fn length(&self, track: NodeId) -> quercus_core::Rational;
}
