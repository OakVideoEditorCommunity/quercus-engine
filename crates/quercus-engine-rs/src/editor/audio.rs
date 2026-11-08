//! Clip audio-state queries for editor UIs.

use std::ffi::c_void;

use crate::node::NodeId;
use crate::types::TimeRange;

use super::{EditorSession, Rational};

/// One clip audio effect-chain entry.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipAudioEffect {
	/// Built-in effect type id.
	pub type_id: String,
	/// Whether the effect is enabled.
	pub enabled: bool,
	/// Static Volume value, when the entry carries one.
	pub volume: Option<f64>,
	/// Static Pan value, when the entry carries one.
	pub pan: Option<f64>,
	/// All static scalar parameters the entry carries, keyed by input id.
	pub params: Vec<(String, f64)>,
}

/// The engine's loudness measurement POD. Layout matches
/// `QuercusLoudness` in the engine C ABI.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct QuercusLoudness {
	/// Gated integrated loudness in LUFS (`-inf` for silence).
	pub integrated_lufs: f64,
	/// Approximate true peak in dBFS (`-inf` for silence).
	pub peak_dbfs: f64,
}

/// A loudness measurement in LUFS/dBFS. Alias of the C-compatible POD.
pub type LoudnessMeasurement = QuercusLoudness;

/// One entry of the current sequence's master-bus effect chain.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioBusEffect {
	/// Built-in effect type id.
	pub type_id: String,
	/// Whether the effect is enabled.
	pub enabled: bool,
	/// All static scalar parameters the entry carries, keyed by input id.
	pub params: Vec<(String, f64)>,
}

/// Per-clip audio mix state.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ClipAudioState {
	/// Gain (1.0 = unity).
	pub gain: f64,
	/// Stereo pan (-1..1).
	pub pan: f64,
	/// Linear fade-in duration.
	pub fade_in: Rational,
	/// Linear fade-out duration.
	pub fade_out: Rational,
	/// Ordered effect chain.
	pub effects: Vec<ClipAudioEffect>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct QuercusClipAudioState {
	gain: f64,
	pan: f64,
	fade_in_num: i64,
	fade_in_den: i64,
	fade_out_num: i64,
	fade_out_den: i64,
	effect_count: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct QuercusClipAudioEffectInfo {
	type_id: [u8; 128],
	enabled: bool,
}

impl Default for QuercusClipAudioEffectInfo {
	fn default() -> Self {
		Self {
			type_id: [0u8; 128],
			enabled: true,
		}
	}
}

#[cfg_attr(
	windows,
	link(name = "quercus_engine.dll.lib", kind = "dylib", modifiers = "+verbatim")
)]
#[cfg_attr(not(windows), link(name = "quercus_engine", kind = "dylib"))]
unsafe extern "C" {
	fn quercus_editor_clip_audio_state(
		session: *const c_void,
		clip: u64,
		out: *mut QuercusClipAudioState,
	) -> bool;
	fn quercus_editor_clip_audio_effect_info(
		session: *const c_void,
		clip: u64,
		index: i32,
		out: *mut QuercusClipAudioEffectInfo,
	) -> bool;
	fn quercus_editor_clip_audio_effect_param(
		session: *const c_void,
		clip: u64,
		index: i32,
		input: *const u8,
		input_len: usize,
		out: *mut f64,
	) -> bool;
	fn quercus_editor_measure_loudness(
		session: *const c_void,
		range: *const TimeRange,
		out: *mut QuercusLoudness,
	) -> bool;
	fn quercus_editor_audio_bus_effect_count(session: *const c_void) -> i32;
	fn quercus_editor_audio_bus_effect_info(
		session: *const c_void,
		index: i32,
		out: *mut QuercusClipAudioEffectInfo,
	) -> bool;
	fn quercus_editor_audio_bus_effect_param(
		session: *const c_void,
		index: i32,
		input: *const u8,
		input_len: usize,
		out: *mut f64,
	) -> bool;
	fn quercus_editor_add_audio_bus_effect(
		session: *const c_void,
		type_id: *const u8,
		type_id_len: usize,
		index: i32,
	) -> bool;
	fn quercus_editor_remove_audio_bus_effect(session: *const c_void, index: i32) -> bool;
	fn quercus_editor_set_audio_bus_effect_enabled(
		session: *const c_void,
		index: i32,
		enabled: bool,
	) -> bool;
	fn quercus_editor_set_audio_bus_effect_param(
		session: *const c_void,
		index: i32,
		input_id: *const u8,
		input_id_len: usize,
		value: f64,
	) -> bool;
}

impl EditorSession {
	/// Read the audio mix state and effect chain of one clip.
	///
	/// `clip` is the clip node's id. Returns `None` when the engine refuses
	/// the query — an invalid/stale node id, a node that is not a clip block
	/// (so no clip behavior to read), or a null session — and `Some` with:
	/// `gain` (1.0 = unity), `pan` (-1..1), `fade_in`/`fade_out` as linear
	/// durations converted from the engine's numerator/denominator pair into
	/// [`Rational`], and the ordered effect chain.
	///
	/// Each effect entry mirrors [`Self::audio_bus_effects`]: type id decoded
	/// from the fixed buffer up to the first NUL, the `enabled` flag, and the
	/// whitelisted static params only. `volume` and `pan` are convenience
	/// copies of the `volume_in`/`panning_in` entries when present, and
	/// `None` otherwise. An entry whose info query fails, or a param whose
	/// query fails, is skipped rather than aborting the whole read, so a
	/// partially unreadable chain still returns the entries that did resolve.
	pub fn clip_audio(&self, clip: NodeId) -> Option<ClipAudioState> {
		let mut raw = QuercusClipAudioState::default();
		unsafe {
			quercus_editor_clip_audio_state(self.ptr, clip.raw(), &mut raw)
		}
		.then_some(())?;
		let mut effects = Vec::new();
		for index in 0..raw.effect_count.max(0) {
			let mut info = QuercusClipAudioEffectInfo::default();
			if !unsafe {
				quercus_editor_clip_audio_effect_info(self.ptr, clip.raw(), index, &mut info)
			} {
				continue;
			}
			let end = info
				.type_id
				.iter()
				.position(|&byte| byte == 0)
				.unwrap_or(info.type_id.len());
			let type_id = String::from_utf8_lossy(&info.type_id[..end]).into_owned();
			let get = |input: &str| {
				let mut value = 0.0f64;
				unsafe {
					quercus_editor_clip_audio_effect_param(
						self.ptr,
						clip.raw(),
						index,
						input.as_ptr(),
						input.len(),
						&mut value,
					)
				}
				.then_some(value)
			};
			let inputs: &[&str] = match type_id.as_str() {
				"org.olivevideoeditor.Olive.volume" => &["volume_in"],
				"org.olivevideoeditor.Olive.pan" => &["panning_in"],
				"org.olivevideoeditor.Olive.eq" => {
					&["low_gain_db", "mid_gain_db", "high_gain_db"]
				}
				"org.olivevideoeditor.Olive.compressor" => &[
					"threshold_db",
					"ratio",
					"attack_ms",
					"release_ms",
					"makeup_db",
				],
				"org.olivevideoeditor.Olive.limiter" => &["ceiling_db", "release_ms"],
				"org.olivevideoeditor.Olive.gate" => {
					&["threshold_db", "attack_ms", "release_ms", "hold_ms"]
				}
				"org.olivevideoeditor.Olive.ducking" => &[
					"threshold_db",
					"amount_db",
					"attack_ms",
					"release_ms",
				],
				_ => &[],
			};
			let mut params = Vec::new();
			for input in inputs {
				if let Some(value) = get(input) {
					params.push((input.to_string(), value));
				}
			}
			let volume = params
				.iter()
				.find(|(input, _)| input == "volume_in")
				.map(|(_, value)| *value);
			let pan = params
				.iter()
				.find(|(input, _)| input == "panning_in")
				.map(|(_, value)| *value);
			effects.push(ClipAudioEffect {
				type_id,
				enabled: info.enabled,
				volume,
				pan,
				params,
			});
		}
		Some(ClipAudioState {
			gain: raw.gain,
			pan: raw.pan,
			fade_in: Rational::new(raw.fade_in_num, raw.fade_in_den),
			fade_out: Rational::new(raw.fade_out_num, raw.fade_out_den),
			effects,
		})
	}

	/// Mixes `range` through the normal preview-audio path and measures
	/// its approximate integrated LUFS and true peak. Returns `None` when
	/// the session has no current sequence or the audio render fails.
	pub fn measure_loudness(&self, range: TimeRange) -> Option<LoudnessMeasurement> {
		let mut out = QuercusLoudness::default();
		unsafe { quercus_editor_measure_loudness(self.ptr, &range, &mut out) }.then_some(out)
	}

	/// Read the current sequence's master-bus effect chain.
	///
	/// Returns one [`AudioBusEffect`] per chain entry in chain order, or an
	/// empty vector when the session has no current sequence (the engine
	/// reports a count of 0). Each entry carries its built-in effect type id
	/// (decoded lossily from the engine's fixed 128-byte buffer, up to the
	/// first NUL), its `enabled` flag, and its static scalar parameters as
	/// `(input id, value)` pairs. Only a per-type whitelist of inputs is
	/// queried — `volume_in`; `panning_in`; `low_gain_db`/`mid_gain_db`/
	/// `high_gain_db`; the compressor's threshold/ratio/attack/release/
	/// makeup; the limiter's ceiling/release; the gate's threshold/attack/
	/// release/hold; the ducking's threshold/amount/attack/release — so an
	/// unknown type id (an entry added while the renderer does not know it)
	/// yields an entry with no params, and a single failed parameter query
	/// simply omits that pair. Entries whose info query fails are skipped,
	/// which can make the returned vector shorter than the engine chain —
	/// the index for [`Self::set_audio_bus_effect_param`] is the engine's,
	/// not this vector's.
	pub fn audio_bus_effects(&self) -> Vec<AudioBusEffect> {
		let count = unsafe { quercus_editor_audio_bus_effect_count(self.ptr) }.max(0);
		let mut effects = Vec::new();
		for index in 0..count {
			let mut info = QuercusClipAudioEffectInfo::default();
			if !unsafe { quercus_editor_audio_bus_effect_info(self.ptr, index, &mut info) } {
				continue;
			}
			let end = info
				.type_id
				.iter()
				.position(|&byte| byte == 0)
				.unwrap_or(info.type_id.len());
			let type_id = String::from_utf8_lossy(&info.type_id[..end]).into_owned();
			let get = |input: &str| {
				let mut value = 0.0f64;
				unsafe {
					quercus_editor_audio_bus_effect_param(
						self.ptr,
						index,
						input.as_ptr(),
						input.len(),
						&mut value,
					)
				}
				.then_some(value)
			};
			let inputs: &[&str] = match type_id.as_str() {
				"org.olivevideoeditor.Olive.volume" => &["volume_in"],
				"org.olivevideoeditor.Olive.pan" => &["panning_in"],
				"org.olivevideoeditor.Olive.eq" => {
					&["low_gain_db", "mid_gain_db", "high_gain_db"]
				}
				"org.olivevideoeditor.Olive.compressor" => &[
					"threshold_db",
					"ratio",
					"attack_ms",
					"release_ms",
					"makeup_db",
				],
				"org.olivevideoeditor.Olive.limiter" => &["ceiling_db", "release_ms"],
				"org.olivevideoeditor.Olive.gate" => {
					&["threshold_db", "attack_ms", "release_ms", "hold_ms"]
				}
				"org.olivevideoeditor.Olive.ducking" => &[
					"threshold_db",
					"amount_db",
					"attack_ms",
					"release_ms",
				],
				_ => &[],
			};
			let mut params = Vec::new();
			for input in inputs {
				if let Some(value) = get(input) {
					params.push((input.to_string(), value));
				}
			}
			effects.push(AudioBusEffect {
				type_id,
				enabled: info.enabled,
				params,
			});
		}
		effects
	}

	/// Appends an effect to the current sequence's master-bus chain at
	/// `index` (clamped to the chain length) as one undo entry.
	pub fn add_audio_bus_effect(&self, type_id: &str, index: usize) -> bool {
		let Ok(index) = i32::try_from(index) else {
			return false;
		};
		unsafe {
			quercus_editor_add_audio_bus_effect(
				self.ptr,
				type_id.as_ptr(),
				type_id.len(),
				index,
			)
		}
	}

	/// Removes one entry of the current sequence's master-bus chain.
	pub fn remove_audio_bus_effect(&self, index: usize) -> bool {
		let Ok(index) = i32::try_from(index) else {
			return false;
		};
		unsafe { quercus_editor_remove_audio_bus_effect(self.ptr, index) }
	}

	/// Enable or disable one entry of the current sequence's master-bus
	/// effect chain, as one undo row.
	///
	/// `index` is the entry's position in the chain (see
	/// [`Self::audio_bus_effects`]); a disabled entry is skipped by the
	/// mixer without being removed from the chain, so the indices of the
	/// following entries are unaffected. `enabled` is written verbatim.
	///
	/// Returns `false` when `index` does not fit in `i32`, the session has
	/// no current sequence, the current node is not a sequence, or `index`
	/// is past the end of the chain; `true` otherwise, including the no-op
	/// case where the entry already had that state (no undo row is pushed
	/// then).
	pub fn set_audio_bus_effect_enabled(&self, index: usize, enabled: bool) -> bool {
		let Ok(index) = i32::try_from(index) else {
			return false;
		};
		unsafe { quercus_editor_set_audio_bus_effect_enabled(self.ptr, index, enabled) }
	}

	/// Set one static (non-keyframed) scalar parameter of a master-bus
	/// effect entry.
	///
	/// `index` indexes the current sequence's master-bus chain (the order
	/// returned by [`Self::audio_bus_effects`]); `input_id` is the effect
	/// input name, e.g. `volume_in`, `panning_in`, `low_gain_db`,
	/// `threshold_db` or `release_ms` (surrounding whitespace is trimmed).
	/// `value` is in the input's own unit (gain for `volume_in`, -1..1 for
	/// pan, dB for the `*_db` inputs, ms for the `*_ms` inputs) and must be
	/// finite — NaN/inf is rejected, not clamped. The write replaces the
	/// entry's static value and is recorded as one undo row; an input id the
	/// entry does not carry yet is appended as a new static value.
	///
	/// Returns `false` (this API has no error type) when `index` does not
	/// fit in `i32`, the session has no current sequence, the current node
	/// is not a sequence, `index` is past the end of the chain, `input_id`
	/// is empty, or `value` is not finite. Returns `true` when the value was
	/// stored or already equal — an unchanged value is a successful no-op
	/// that pushes no undo row.
	pub fn set_audio_bus_effect_param(
		&self,
		index: usize,
		input_id: &str,
		value: f64,
	) -> bool {
		let Ok(index) = i32::try_from(index) else {
			return false;
		};
		unsafe {
			quercus_editor_set_audio_bus_effect_param(
				self.ptr,
				index,
				input_id.as_ptr(),
				input_id.len(),
				value,
			)
		}
	}
}
