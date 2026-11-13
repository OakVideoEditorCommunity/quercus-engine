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

//! The products of describe: the effect descriptor and the clip descriptor.
//!
//! Reference: HS: ofxhImageEffect.cpp `Descriptor` (the property container
//! that plugins can read and write during the describe action).
//!
//! Handle convention ([`crate::suites::tag`]): `props` sits at offset 0 and
//! is `#[repr(C)]`; `params`/`clips` elements are boxed (a Vec reallocation
//! does not move the objects).

use std::ffi::CString;

use crate::param::ParamDef;
use crate::property::{PropertySet, Value};

fn cs(s: &str) -> CString {
	CString::new(s).unwrap()
}

// ---- clip 描述符属性（ofxImageEffect.h / ofxCore.h）----

/// kOfxImageClipPropOptional.
pub(crate) const CLIP_OPTIONAL: &str = "OfxImageClipPropOptional";
/// kOfxImageClipPropIsMask.
pub(crate) const CLIP_IS_MASK: &str = "OfxImageClipPropIsMask";
/// kOfxImageClipPropFieldExtraction.
pub(crate) const CLIP_FIELD_EXTRACTION: &str = "OfxImageClipPropFieldExtraction";
/// kOfxImageEffectPropSupportedComponents.
pub(crate) const CLIP_SUPPORTED_COMPONENTS: &str = "OfxImageEffectPropSupportedComponents";
/// kOfxImageEffectPropTemporalClipAccess.
pub(crate) const CLIP_TEMPORAL_ACCESS: &str = "OfxImageEffectPropTemporalClipAccess";
/// kOfxImageEffectPropSupportsTiles.
pub(crate) const CLIP_SUPPORTS_TILES: &str = "OfxImageEffectPropSupportsTiles";
/// kOfxImageFieldDoubled (the FieldExtraction default).
pub(crate) const CLIP_FIELD_DOUBLED: &str = "OfxImageFieldDoubled";
/// kOfxTypeClip.
pub(crate) const CLIP_TYPE: &str = "OfxTypeClip";

/// Clip descriptor (defined by the plugin during describe).
/// `#[repr(C)]` + props at offset 0 (the handle convention; during describe
/// the clip handle is &props, sharing the CLIP tag with the instance-phase
/// [`crate::clip::ClipInstance`]).
#[repr(C)]
pub struct ClipDescriptor {
	/// Descriptive properties: label, optionality (kOfxImageClipPropOptional),
	/// supported components (kOfxImageEffectPropSupportedComponents), field
	/// support, and so on.
	pub props: PropertySet,
	/// The clip name (e.g. "Source"/"Output").
	pub name: String,
}

impl ClipDescriptor {
	/// Build by name (mirrors HS ofxhClip.cpp `ClipDescriptor::ClipDescriptor`
	/// plus the clipDescriptorStuffs property table, ofxhClip.cpp:22-38).
	/// Public: tests construct clip instances with it (GL suite round-trip).
	pub fn new(name: &str) -> Self {
		let props = PropertySet::new();
		props.set_one(crate::param::PROP_TYPE, Value::String(cs(CLIP_TYPE)));
		props.set_one(crate::param::PROP_NAME, Value::String(cs(name)));
		props.set_one(crate::param::PROP_LABEL, Value::String(cs(name)));
		props.set_one(crate::param::PROP_SHORT_LABEL, Value::String(cs("")));
		props.set_one(crate::param::PROP_LONG_LABEL, Value::String(cs("")));
		props.define(CLIP_SUPPORTED_COMPONENTS, vec![]);
		props.set_one(CLIP_TEMPORAL_ACCESS, Value::Int(0));
		props.set_one(CLIP_OPTIONAL, Value::Int(0));
		props.set_one(CLIP_IS_MASK, Value::Int(0));
		props.set_one(CLIP_FIELD_EXTRACTION, Value::String(cs(CLIP_FIELD_DOUBLED)));
		// kOfxImageClipPropFieldOrder（ofxhClip.cpp 的 clip 描述符必备
		// 属性；ofxs `Clip::getFieldOrder()` 无默认值强读——Mirror 这类
		// 场感知插件缺它直接 MissingHostFeature）。取值是
		// kOfxImageField* 族（ofxImageEffect.h:1288），默认无场。
		props.set_one(
			"OfxImageClipPropFieldOrder",
			Value::String(cs("OfxFieldNone")),
		);
		props.set_one(CLIP_SUPPORTS_TILES, Value::Int(1));
		// ofxColour（M11 §4）：clip 色彩空间属性族。Colourspace 由宿主
		// 在实例化时写入（输入 clip = 工作空间 ACEScg）；Preferred 由
		// 插件在 GetClipPreferences 写（宿主侧预置空数组）。
		props.set_one(crate::host::PROP_CLIP_COLOURSPACE, Value::String(cs("")));
		props.define(crate::host::PROP_CLIP_PREFERRED_COLOURSPACES, vec![]);
		Self {
			props,
			name: name.to_string(),
		}
	}
}

/// Effect descriptor.
/// `#[repr(C)]` + props at offset 0 (the handle convention: during describe
/// the effect/param-set handle is &props).
#[repr(C)]
pub struct EffectDescriptor {
	/// Effect-level properties: label, description, grouping, single-frame/
	/// temporal flags, and so on.
	pub props: PropertySet,
	/// Parameter definition set (filled by the plugin through the Param suite
	/// during describe; for the value types see [`crate::param::ParamDef`];
	/// boxing keeps handle addresses stable).
	pub params: Vec<Box<ParamDef>>,
	/// Clip definition set (includes "Output"; boxing keeps handle addresses
	/// stable).
	pub clips: Vec<Box<ClipDescriptor>>,
}

impl EffectDescriptor {
	/// An empty descriptor (created by the host before the describe action, with
	/// the root properties pre-set).
	pub fn new() -> Self {
		Self {
			props: PropertySet::new(),
			params: Vec::new(),
			clips: Vec::new(),
		}
	}

	/// Look up an input/output clip definition by clip name.
	///
	/// Linear search; returns the first definition whose name matches exactly
	/// (`"Output"` included). `None` when nothing matches. Compare
	/// [`Self::param`], which searches the parameter table instead.
	///
	/// # Arguments
	///
	/// * `name` - the clip name (in the `kOfxImageEffectPropClipName` sense,
	///   e.g. `"Source"`, `"Output"`).
	pub fn clip(&self, name: &str) -> Option<&ClipDescriptor> {
		self.clips
			.iter()
			.find(|c| c.name == name)
			.map(|b| b.as_ref())
	}

	/// Look up a parameter definition by parameter name (the definition the
	/// plugin filled in during describe, not an instance value).
	///
	/// Linear search; returns the first definition whose name matches exactly;
	/// `None` when nothing matches (case-sensitive). Runtime parameter values
	/// live on the instance side (`find` on
	/// [`crate::instance::Instance::params`]); do not read them with this method.
	///
	/// # Arguments
	///
	/// * `name` - the parameter's internal name (`kOfxParamPropName`, not the
	///   display label).
	pub fn param(&self, name: &str) -> Option<&ParamDef> {
		self.params
			.iter()
			.find(|p| p.name == name)
			.map(|b| b.as_ref())
	}
}
