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

//! Parameter system: 13 parameter instances + the param ↔ quercusnode bridge.
//!
//! Mirrors C++'s `ParamInstance`/`OliveParamInstance`. Bridge semantics
//! (settled in M9): node input value change → write back to the OFX parameter; an OFX parameter
//! changed by the plugin → write back to the node through quercusnode (undoable via
//! `quercus_undo::undocommand::UndoCommand`).
//!
//! Handle conventions ([`crate::suites::tag`]): `ParamDef`/`ParamInstance`
//! are `#[repr(C)]` with `props` at offset 0; element boxing (`Vec<Box<..>>`) guarantees
//! that Vec reallocation moves no object and no handle dangles.
//!
//! Node value layout: frozen together with [`crate::node`] (the
//! `quercusnode_value` POD in `include/node/node.h`); after the single-lib unification quercusnode's identity registry was
//! deleted with its ffi, and the write-back path is a local stub that keeps the failure path (see [`crate::node`]).

use std::ffi::CString;

use crate::property::{PropertySet, Value};

// ---- OFX 参数类型字符串（ofxParam.h）----

/// kOfxParamTypeInteger.
pub const TYPE_INTEGER: &str = "OfxParamTypeInteger";
/// kOfxParamTypeInteger2D.
pub const TYPE_INTEGER2D: &str = "OfxParamTypeInteger2D";
/// kOfxParamTypeInteger3D.
pub const TYPE_INTEGER3D: &str = "OfxParamTypeInteger3D";
/// kOfxParamTypeDouble.
pub const TYPE_DOUBLE: &str = "OfxParamTypeDouble";
/// kOfxParamTypeDouble2D.
pub const TYPE_DOUBLE2D: &str = "OfxParamTypeDouble2D";
/// kOfxParamTypeDouble3D.
pub const TYPE_DOUBLE3D: &str = "OfxParamTypeDouble3D";
/// kOfxParamTypeBoolean.
pub const TYPE_BOOLEAN: &str = "OfxParamTypeBoolean";
/// kOfxParamTypeChoice.
pub const TYPE_CHOICE: &str = "OfxParamTypeChoice";
/// kOfxParamTypeString.
pub const TYPE_STRING: &str = "OfxParamTypeString";
/// kOfxParamTypeStrChoice.
pub const TYPE_STRCHOICE: &str = "OfxParamTypeStrChoice";
/// kOfxParamTypeRGB.
pub const TYPE_RGB: &str = "OfxParamTypeRGB";
/// kOfxParamTypeRGBA.
pub const TYPE_RGBA: &str = "OfxParamTypeRGBA";
/// kOfxParamTypeBytes.
pub const TYPE_BYTES: &str = "OfxParamTypeBytes";
/// kOfxParamTypeCustom.
pub const TYPE_CUSTOM: &str = "OfxParamTypeCustom";
/// kOfxParamTypePushButton.
pub const TYPE_PUSHBUTTON: &str = "OfxParamTypePushButton";
/// kOfxParamTypeGroup.
pub const TYPE_GROUP: &str = "OfxParamTypeGroup";
/// kOfxParamTypePage.
pub const TYPE_PAGE: &str = "OfxParamTypePage";
/// kOfxParamTypeParametric (value = curve list, read/written through
/// OfxParametricParameterSuite, see
/// [`crate::suites::parametric`] and [`crate::param_curve`]).
pub const TYPE_PARAMETRIC: &str = "OfxParamTypeParametric";

// ---- 属性名（ofxParam.h / ofxCore.h）----

/// kOfxParamPropSecret.
pub(crate) const P_SECRET: &str = "OfxParamPropSecret";
/// kOfxParamPropHint.
pub(crate) const P_HINT: &str = "OfxParamPropHint";
/// kOfxParamPropScriptName.
pub(crate) const P_SCRIPT_NAME: &str = "OfxParamPropScriptName";
/// kOfxParamPropParent.
pub(crate) const P_PARENT: &str = "OfxParamPropParent";
/// kOfxParamPropEnabled.
pub(crate) const P_ENABLED: &str = "OfxParamPropEnabled";
/// kOfxParamPropDataPtr.
pub(crate) const P_DATA_PTR: &str = "OfxParamPropDataPtr";
/// kOfxParamPropType.
pub(crate) const P_TYPE: &str = "OfxParamPropType";
/// kOfxParamPropIsAnimating.
pub(crate) const P_IS_ANIMATING: &str = "OfxParamPropIsAnimating";
/// kOfxParamPropIsAutoKeying.
pub(crate) const P_IS_AUTO_KEYING: &str = "OfxParamPropIsAutoKeying";
/// kOfxParamPropPersistant.
pub(crate) const P_PERSISTANT: &str = "OfxParamPropPersistant";
/// kOfxParamPropEvaluateOnChange.
pub(crate) const P_EVALUATE_ON_CHANGE: &str = "OfxParamPropEvaluateOnChange";
/// kOfxParamPropCanUndo.
pub(crate) const P_CAN_UNDO: &str = "OfxParamPropCanUndo";
/// kOfxParamPropCacheInvalidation.
pub(crate) const P_CACHE_INVALIDATION: &str = "OfxParamPropCacheInvalidation";
/// kOfxParamInvalidateValueChange (the default value of P_CACHE_INVALIDATION).
pub(crate) const V_INVALIDATE_VALUE_CHANGE: &str = "OfxParamInvalidateValueChange";
/// kOfxParamPropAnimates.
pub(crate) const P_ANIMATES: &str = "OfxParamPropAnimates";
/// kOfxParamPropDefault.
pub(crate) const P_DEFAULT: &str = "OfxParamPropDefault";
/// kOfxParamPropDisplayMin.
pub(crate) const P_DISPLAY_MIN: &str = "OfxParamPropDisplayMin";
/// kOfxParamPropDisplayMax.
pub(crate) const P_DISPLAY_MAX: &str = "OfxParamPropDisplayMax";
/// kOfxParamPropMin.
pub(crate) const P_MIN: &str = "OfxParamPropMin";
/// kOfxParamPropMax.
pub(crate) const P_MAX: &str = "OfxParamPropMax";
/// kOfxParamPropIncrement.
pub(crate) const P_INCREMENT: &str = "OfxParamPropIncrement";
/// kOfxParamPropDigits.
pub(crate) const P_DIGITS: &str = "OfxParamPropDigits";
/// kOfxParamPropDoubleType.
pub(crate) const P_DOUBLE_TYPE: &str = "OfxParamPropDoubleType";
/// kOfxParamDoubleTypePlain.
pub(crate) const V_DOUBLE_TYPE_PLAIN: &str = "OfxParamDoubleTypePlain";
/// kOfxParamPropDefaultCoordinateSystem.
pub(crate) const P_DEFAULT_COORD_SYS: &str = "OfxParamPropDefaultCoordinateSystem";
/// kOfxParamCoordinatesCanonical.
pub(crate) const V_COORD_CANONICAL: &str = "OfxParamCoordinatesCanonical";
/// kOfxParamCoordinatesNormalised (ofxParam.h:514).
pub(crate) const V_COORD_NORMALISED: &str = "OfxParamCoordinatesNormalised";
/// kOfxParamPropShowTimeMarker.
pub(crate) const P_SHOW_TIME_MARKER: &str = "OfxParamPropShowTimeMarker";
/// kOfxParamPropDimensionLabel.
pub(crate) const P_DIMENSION_LABEL: &str = "OfxParamPropDimensionLabel";
/// kOfxParamPropStringMode.
pub(crate) const P_STRING_MODE: &str = "OfxParamPropStringMode";
/// kOfxParamStringIsSingleLine.
pub(crate) const V_STRING_SINGLE_LINE: &str = "OfxParamStringIsSingleLine";
/// kOfxParamPropStringFilePathExists.
pub(crate) const P_STRING_FILE_EXISTS: &str = "OfxParamPropStringFilePathExists";
/// kOfxParamPropChoiceOption.
pub(crate) const P_CHOICE_OPTION: &str = "OfxParamPropChoiceOption";
/// kOfxParamPropChoiceOrder.
pub(crate) const P_CHOICE_ORDER: &str = "OfxParamPropChoiceOrder";
/// kOfxParamPropChoiceEnum.
pub(crate) const P_CHOICE_ENUM: &str = "OfxParamPropChoiceEnum";
/// kOfxParamPageSkipRow (page-child sentinel, ofxParam.h:178).
pub(crate) const PAGE_SKIP_ROW: &str = "OfxParamPageSkipRow";
/// kOfxParamPageSkipColumn (page-child sentinel, ofxParam.h:186).
pub(crate) const PAGE_SKIP_COLUMN: &str = "OfxParamPageSkipColumn";
/// kOfxParamPropCustomInterpCallbackV1.
pub(crate) const P_CUSTOM_INTERP: &str = "OfxParamPropCustomCallbackV1";
/// kOfxParamPropParametricDimension (int×1, default 1).
pub(crate) const P_PARAMETRIC_DIMENSION: &str = "OfxParamPropParametricDimension";
/// kOfxParamPropParametricRange (double×2, default (0,1)).
pub(crate) const P_PARAMETRIC_RANGE: &str = "OfxParamPropParametricRange";
/// kOfxParamPropParametricUIColour (double×3N, unset by default).
pub(crate) const P_PARAMETRIC_UI_COLOUR: &str = "OfxParamPropParametricUIColour";
/// kOfxParamPropParametricInteractBackground (pointer×1, default NULL).
pub(crate) const P_PARAMETRIC_INTERACT_BG: &str = "OfxParamPropParametricInteractBackground";
/// kOfxParamPropPageChild.
pub(crate) const P_PAGE_CHILD: &str = "OfxParamPropPageChild";
/// kOfxParamPropGroupOpen.
pub(crate) const P_GROUP_OPEN: &str = "OfxParamPropGroupOpen";
/// kOfxParamPropInteractV1.
pub(crate) const P_INTERACT_V1: &str = "OfxParamPropInteractV1";
/// kOfxParamPropInteractSize.
pub(crate) const P_INTERACT_SIZE: &str = "OfxParamPropInteractSize";
/// kOfxParamPropInteractSizeAspect.
pub(crate) const P_INTERACT_ASPECT: &str = "OfxParamPropInteractSizeAspect";
/// kOfxParamPropInteractMinimumSize.
pub(crate) const P_INTERACT_MIN_SIZE: &str = "OfxParamPropInteractMinimumSize";
/// kOfxParamPropInteractPreferedSize.
pub(crate) const P_INTERACT_PREF_SIZE: &str = "OfxParamPropInteractPreferedSize";
/// kOfxPropType.
pub(crate) const PROP_TYPE: &str = "OfxPropType";
/// kOfxPropName.
pub(crate) const PROP_NAME: &str = "OfxPropName";
/// kOfxPropLabel.
pub(crate) const PROP_LABEL: &str = "OfxPropLabel";
/// kOfxPropShortLabel.
pub(crate) const PROP_SHORT_LABEL: &str = "OfxPropShortLabel";
/// kOfxPropLongLabel.
pub(crate) const PROP_LONG_LABEL: &str = "OfxPropLongLabel";
/// kOfxPropIcon.
pub(crate) const PROP_ICON: &str = "OfxPropIcon";
/// kOfxTypeParameter.
pub(crate) const TYPE_PARAMETER: &str = "OfxTypeParameter";

fn cs(s: &str) -> CString {
	// 静态 ASCII 常量，无内嵌 NUL。
	CString::new(s).unwrap()
}

/// Parameter value kind (the param shim's varargs dispatch; corresponds
/// verbatim to the KIND_* enum in cbits/ofx_param_shim.c).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
	/// Integer.
	Int = 1,
	/// Integer2D.
	Int2 = 2,
	/// Integer3D.
	Int3 = 3,
	/// Double.
	Double = 4,
	/// Double2D.
	Double2 = 5,
	/// Double3D.
	Double3 = 6,
	/// Boolean.
	Bool = 7,
	/// Choice.
	Choice = 8,
	/// RGB.
	Rgb = 9,
	/// RGBA.
	Rgba = 10,
	/// String.
	Str = 11,
	/// StrChoice.
	StrChoice = 12,
}

/// OFX type string → value kind + dimension (HS: ofxhParam.cpp `findType`).
/// Value-less types (PushButton/Group/Page) and parametric (curve values go
/// through OfxParametricParameterSuite, which has no scalar varargs entry) return None.
pub(crate) fn kind_of_type(ofx_type: &str) -> Option<(ParamKind, usize)> {
	match ofx_type {
		TYPE_INTEGER => Some((ParamKind::Int, 1)),
		TYPE_INTEGER2D => Some((ParamKind::Int2, 2)),
		TYPE_INTEGER3D => Some((ParamKind::Int3, 3)),
		TYPE_DOUBLE => Some((ParamKind::Double, 1)),
		TYPE_DOUBLE2D => Some((ParamKind::Double2, 2)),
		TYPE_DOUBLE3D => Some((ParamKind::Double3, 3)),
		TYPE_BOOLEAN => Some((ParamKind::Bool, 1)),
		TYPE_CHOICE => Some((ParamKind::Choice, 1)),
		TYPE_STRING => Some((ParamKind::Str, 1)),
		TYPE_STRCHOICE => Some((ParamKind::StrChoice, 1)),
		TYPE_RGB => Some((ParamKind::Rgb, 3)),
		TYPE_RGBA => Some((ParamKind::Rgba, 4)),
		_ => None,
	}
}

/// Numeric parameter types (the Min/Max/DisplayMin/Max tables apply; HS
/// `addNumericParamProps`'s isDoubleParam || isIntParam ||
/// isColourParam).
fn is_numeric_type(ofx_type: &str) -> bool {
	matches!(
		ofx_type,
		TYPE_INTEGER
			| TYPE_INTEGER2D
			| TYPE_INTEGER3D
			| TYPE_DOUBLE
			| TYPE_DOUBLE2D
			| TYPE_DOUBLE3D
			| TYPE_RGB
			| TYPE_RGBA
	)
}

/// The default ParamValue of this type (the describe-time baseline; HS keeps the property default
/// in props' kOfxParamPropDefault, with equivalent value semantics).
fn type_default(ofx_type: &str) -> ParamValue {
	match kind_of_type(ofx_type) {
		Some((ParamKind::Int, d)) | Some((ParamKind::Int2, d)) | Some((ParamKind::Int3, d)) => {
			ParamValue::Int([0; 3], d)
		}
		Some((ParamKind::Double, d))
		| Some((ParamKind::Double2, d))
		| Some((ParamKind::Double3, d)) => ParamValue::Double([0.0; 3], d),
		Some((ParamKind::Rgb, d)) | Some((ParamKind::Rgba, d)) => ParamValue::Color([0.0; 4], d),
		Some((ParamKind::Bool, _)) => ParamValue::Bool(false),
		Some((ParamKind::Choice, _)) => ParamValue::Choice(0),
		Some((ParamKind::Str, _)) => ParamValue::String(cs("")),
		Some((ParamKind::StrChoice, _)) => ParamValue::StrChoice(cs("")),
		None => match ofx_type {
			TYPE_BYTES | TYPE_CUSTOM => ParamValue::Bytes(Vec::new()),
			TYPE_PUSHBUTTON => ParamValue::PushButton,
			TYPE_GROUP | TYPE_PAGE => ParamValue::Container,
			// parametric 默认 = 1 条恒等曲线（range 默认 (0,1)）。
			TYPE_PARAMETRIC => ParamValue::Parametric(vec![crate::param_curve::Curve::identity(
				0.0, 1.0,
			)]),
			_ => ParamValue::Container, // 未知类型占位（paramDefine 已拒绝）
		},
	}
}

/// A value for the numeric property tables (Min/Max/Display family; HS ofxhParam.cpp:420-449).
/// `is_max` picks ±f64::MAX / ±i32::MAX; colour parameters are always 0/1.
fn numeric_value(ofx_type: &str, kind: ParamKind, dim: usize, is_max: bool) -> Vec<Value> {
	if ofx_type == TYPE_RGB || ofx_type == TYPE_RGBA {
		// colour：display 范围 0..1，min/max 同。
		let v = Value::Double(if is_max { 1.0 } else { 0.0 });
		return vec![v; dim];
	}
	match kind {
		ParamKind::Double | ParamKind::Double2 | ParamKind::Double3 => {
			let v = Value::Double(if is_max { f64::MAX } else { f64::MIN });
			vec![v; dim]
		}
		_ => {
			let v = Value::Int(if is_max { i32::MAX } else { i32::MIN });
			vec![v; dim]
		}
	}
}

/// A parameter value (one-to-one with the OFX parameter types).
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
	/// kOfxParamTypeInteger / Integer2D / Integer3D.
	Int([i32; 3], usize),
	/// kOfxParamTypeDouble / Double2D / Double3D.
	Double([f64; 3], usize),
	/// kOfxParamTypeBoolean.
	Bool(bool),
	/// kOfxParamTypeChoice (option index).
	Choice(i32),
	/// kOfxParamTypeString.
	String(std::ffi::CString),
	/// kOfxParamTypeRGB / RGBA.
	Color([f64; 4], usize),
	/// kOfxParamTypeStrChoice.
	StrChoice(std::ffi::CString),
	/// kOfxParamTypeBytes / Custom (opaque byte string).
	Bytes(Vec<u8>),
	/// kOfxParamTypePushButton (no value, trigger only).
	PushButton,
	/// kOfxParamTypeGroup / Page (container, no value).
	Container,
	/// kOfxParamTypeParametric: one curve per dimension (curve model in
	/// [`crate::param_curve`]; the default is 1 identity curve, the dimension comes from the
	/// `OfxParamPropParametricDimension` property, see
	/// [`ParamDef::new`]).
	Parametric(Vec<crate::param_curve::Curve>),
}

/// A parameter definition (a describe product, see [`crate::descriptor::EffectDescriptor`]).
/// `#[repr(C)]` + props at offset 0 (handle convention, see [`crate::suites::tag`]).
#[derive(Clone)]
#[repr(C)]
pub struct ParamDef {
	/// Definition properties (label/hint/parent/coordinate-system/secret/
	/// display min-max/choice options and ordering, etc.; pre-populated at describe time with
	/// the same tables as HostSupport).
	pub props: PropertySet,
	/// Parameter name (the OFX identifier).
	pub name: String,
	/// OFX type string (kOfxParamType*).
	pub ofx_type: String,
	/// Default value (read after describe).
	pub default: ParamValue,
}

impl ParamDef {
	/// Build a parameter definition from an OFX type string and pre-populate the type's standard property tables.
	///
	/// Mirrors HS `ofxhParam.cpp:224-352`; writes into `props` in four layers:
	/// 1. universalProps — `OfxParamPropType=kOfxParamTypeParameter`,
	///    [`PROP_NAME`]/[`PROP_LABEL`]/short label/long label/script name all take
	///    `name`; secret=0, hint empty, parent empty, enabled=1,
	///    data pointer null, icon two empty strings;
	/// 2. value-type invariants (when `kind_of_type` yields a value) — isAnimating=0,
	///    isAutoKeying=0, persistant=1, evaluateOnChange=1, canUndo=1,
	///    cache invalidation=kInvalidateValueChange, Animates per HS rules (custom/
	///    string/boolean/choice are 0, all others 1), the default value uses
	///    [`default_values`];
	/// 3. numeric types (`is_numeric_type`) — the four display/min/max tables use
	///    [`numeric_value`] (±`f64::MAX`/±`i32::MAX` sentinels, colours 0..1),
	///    the double family gets increment=1.0/digits=2, plain type and canonical
	///    coordinate system, 2D/3D append x/y/z dimension labels;
	/// 4. type-specific (HS `addStandardParamProps`) — string single-line mode and
	///    file-exists=1, choice pre-defines three empty option tables (so plugins never read an
	///    unknown property), custom's interpolation pointer, the page child table, group open by default,
	///    parametric's dimension=1/empty interact background/range (0,1); non-group/page types also get
	///    the interact properties.
	///
	/// `name` is the OFX parameter identifier, stored verbatim without dedup or escaping; `ofx_type`
	/// determines the `ofx_type` field and all type properties. Unknown types do not error; a definition
	/// is still returned (`default` degrades to the `ParamValue::Container` placeholder), and the describe
	/// layer is responsible for rejecting it. The default current value comes from `type_default` (e.g. parametric is
	/// one (0,1) identity curve), stored separately from the property default.
	pub(crate) fn new(name: &str, ofx_type: &str) -> Self {
		let props = PropertySet::new();
		let uname = name.to_string();
		let utype = ofx_type.to_string();

		// universalProps（HS ofxhParam.cpp:229-246）。
		props.set_one(PROP_TYPE, Value::String(cs(TYPE_PARAMETER)));
		props.set_one(P_SECRET, Value::Int(0));
		props.set_one(P_HINT, Value::String(cs("")));
		props.set_one(P_SCRIPT_NAME, Value::String(cs(&uname)));
		props.set_one(P_PARENT, Value::String(cs("")));
		props.set_one(P_ENABLED, Value::Int(1));
		props.set_one(P_DATA_PTR, Value::Pointer(std::ptr::null_mut()));
		props.set_one(P_TYPE, Value::String(cs(&utype)));
		props.set_one(PROP_NAME, Value::String(cs(&uname)));
		props.set_one(PROP_LABEL, Value::String(cs(&uname)));
		props.set_one(PROP_SHORT_LABEL, Value::String(cs(&uname)));
		props.set_one(PROP_LONG_LABEL, Value::String(cs(&uname)));
		props.define(
			PROP_ICON,
			vec![Value::String(cs("")), Value::String(cs(""))],
		);

		// 值类参数（HS addValueParamProps，ofxhParam.cpp:352-380）。
		if let Some((kind, dim)) = kind_of_type(ofx_type) {
			props.set_one(P_IS_ANIMATING, Value::Int(0));
			props.set_one(P_IS_AUTO_KEYING, Value::Int(0));
			props.set_one(P_PERSISTANT, Value::Int(1));
			props.set_one(P_EVALUATE_ON_CHANGE, Value::Int(1));
			props.set_one(P_CAN_UNDO, Value::Int(1));
			props.set_one(
				P_CACHE_INVALIDATION,
				Value::String(cs(V_INVALIDATE_VALUE_CHANGE)),
			);
			// 可动画性（HS ofxhParam.cpp:367-372：custom/string/
			// boolean/choice 默认不可动画）。
			let animates = ofx_type != TYPE_CUSTOM
				&& ofx_type != TYPE_STRING
				&& ofx_type != TYPE_BOOLEAN
				&& ofx_type != TYPE_CHOICE;
			props.set_one(P_ANIMATES, Value::Int(animates as i32));
			props.define(P_DEFAULT, default_values(kind, dim));

			// 数值类（HS addNumericParamProps，ofxhParam.cpp:391-455）。
			if is_numeric_type(ofx_type) {
				props.define(P_DISPLAY_MIN, numeric_value(ofx_type, kind, dim, false));
				props.define(P_DISPLAY_MAX, numeric_value(ofx_type, kind, dim, true));
				props.define(P_MIN, numeric_value(ofx_type, kind, dim, false));
				props.define(P_MAX, numeric_value(ofx_type, kind, dim, true));
				if matches!(
					kind,
					ParamKind::Double | ParamKind::Double2 | ParamKind::Double3
				) {
					props.set_one(P_INCREMENT, Value::Double(1.0));
					props.set_one(P_DIGITS, Value::Int(2));
				}
				if ofx_type == TYPE_DOUBLE || ofx_type == TYPE_DOUBLE2D || ofx_type == TYPE_DOUBLE3D
				{
					props.set_one(P_DOUBLE_TYPE, Value::String(cs(V_DOUBLE_TYPE_PLAIN)));
					props.set_one(P_DEFAULT_COORD_SYS, Value::String(cs(V_COORD_CANONICAL)));
					if dim == 1 {
						props.set_one(P_SHOW_TIME_MARKER, Value::Int(0));
					}
				}
				if dim == 2 || dim == 3 {
					let labels: Vec<Value> = ["x", "y", "z"][..dim]
						.iter()
						.map(|l| Value::String(cs(l)))
						.collect();
					props.define(P_DIMENSION_LABEL, labels);
				}
			}
		}

		// 分类型附加（HS addStandardParamProps，ofxhParam.cpp:248-301）。
		match ofx_type {
			TYPE_STRING => {
				props.set_one(P_STRING_MODE, Value::String(cs(V_STRING_SINGLE_LINE)));
				props.set_one(P_STRING_FILE_EXISTS, Value::Int(1));
			}
			TYPE_CHOICE => {
				// 维度 0：选项数由插件 SetN 决定。ChoiceEnum（枚举值映射）
				// 与 ChoiceLabelOption（OFX 1.5 按值标签）同样预定义空
				// 数组——新支持库读它们时不是"未知属性"（其强读会抛成
				// PropertyUnknownToHost → MissingHostFeature）。
				props.define(P_CHOICE_OPTION, vec![]);
				props.define("OfxParamPropChoiceEnum", vec![]);
				props.define("OfxParamPropChoiceLabelOption", vec![]);
			}
			TYPE_CUSTOM => {
				props.set_one(P_CUSTOM_INTERP, Value::Pointer(std::ptr::null_mut()));
			}
			TYPE_PAGE => {
				props.define(P_PAGE_CHILD, vec![]);
			}
			TYPE_GROUP => {
				props.set_one(P_GROUP_OPEN, Value::Int(1));
			}
			TYPE_PARAMETRIC => {
				// parametric（HS addValueParamProps(eDouble, 0) 的
				// invariantProps + allParametric，ofxhParam.cpp:285-330）：
				// 值类 invariant 属性照旧；Animates=1（HS 的 animates 计算
				// 对 parametric 为真）；另加 4 个 parametric 专属属性。
				// 数值属性表（Min/Max/Display）HS 也加（变量维 double），
				// 但插件侧从不读 parametric 的数值属性（ofxsPropertyValidation
				// 只校验本块 4 个 + 通用属性），本 crate 遵循
				// is_numeric_type 的门控，不为 parametric 预置。
				props.set_one(P_IS_ANIMATING, Value::Int(0));
				props.set_one(P_IS_AUTO_KEYING, Value::Int(0));
				props.set_one(P_PERSISTANT, Value::Int(1));
				props.set_one(P_EVALUATE_ON_CHANGE, Value::Int(1));
				props.set_one(P_CAN_UNDO, Value::Int(1));
				props.set_one(
					P_CACHE_INVALIDATION,
					Value::String(cs(V_INVALIDATE_VALUE_CHANGE)),
				);
				props.set_one(P_ANIMATES, Value::Int(1));
				props.set_one(P_PARAMETRIC_DIMENSION, Value::Int(1));
				// UIColour 默认未设：不预定义（插件首写时按 OFX 语义
				// 隐式创建并允许逐位追加；预定义空数组会让 index >= 1
				// 的写入炸 BadIndex——ColorLookup/HueCorrect 的
				// describeInContext 失败根因）。
				props.set_one(P_PARAMETRIC_INTERACT_BG, Value::Pointer(std::ptr::null_mut()));
				props.define(
					P_PARAMETRIC_RANGE,
					vec![Value::Double(0.0), Value::Double(1.0)],
				);
			}
			_ => {}
		}

		// 交互属性（HS addInteractParamProps，ofxhParam.cpp:305-317；
		// group/page 除外）。
		if ofx_type != TYPE_GROUP && ofx_type != TYPE_PAGE {
			props.set_one(P_INTERACT_V1, Value::Pointer(std::ptr::null_mut()));
			props.define(
				P_INTERACT_SIZE,
				vec![Value::Double(0.0), Value::Double(0.0)],
			);
			props.set_one(P_INTERACT_ASPECT, Value::Double(1.0));
			props.define(
				P_INTERACT_MIN_SIZE,
				vec![Value::Double(10.0), Value::Double(10.0)],
			);
			props.define(P_INTERACT_PREF_SIZE, vec![Value::Int(10), Value::Int(10)]);
		}

		Self {
			props,
			name: uname,
			ofx_type: utype,
			default: type_default(ofx_type),
		}
	}

	/// Value kind (used for varargs dispatch; value-less types return None).
	pub(crate) fn kind(&self) -> Option<ParamKind> {
		kind_of_type(&self.ofx_type).map(|(k, _)| k)
	}
}

/// Compute the default-value vector for `kOfxParamPropDefault` (HS ofxhParam.cpp:376-377).
///
/// `kind` decides the element type: the integer family (`Int`/`Int2`/`Int3`/`Bool`/`Choice`)
/// yields `Value::Int(0)`; the float and colour families (`Double*`/`Rgb`/`Rgba`) yield
/// `Value::Double(0.0)`; the string family (`Str`/`StrChoice`) always has exactly 1
/// empty-string element and ignores `dim`.
///
/// `dim` is the type's value dimension: 1/2/3 per [`kind_of_type`]'s convention,
/// 3 for RGB colours and 4 for RGBA; the vector length is `dim.max(1)`, i.e. passing 0
/// degrades to one element (HS's fallback for dimension 0). The result is used by [`ParamDef::new`]
/// to pre-populate the default property; it does not affect an instance's current value (that comes from `type_default`).
fn default_values(kind: ParamKind, dim: usize) -> Vec<Value> {
	match kind {
		ParamKind::Int
		| ParamKind::Int2
		| ParamKind::Int3
		| ParamKind::Bool
		| ParamKind::Choice => {
			vec![Value::Int(0); dim.max(1)]
		}
		ParamKind::Double
		| ParamKind::Double2
		| ParamKind::Double3
		| ParamKind::Rgb
		| ParamKind::Rgba => vec![Value::Double(0.0); dim.max(1)],
		ParamKind::Str | ParamKind::StrChoice => vec![Value::String(cs(""))],
	}
}

/// A parameter instance (instantiated from [`ParamDef`] after createInstance).
/// `#[repr(C)]` + props at offset 0 (handle convention).
#[repr(C)]
pub struct ParamInstance {
	/// Instance-level properties (the current-value mirror, etc.).
	pub props: PropertySet,
	/// The corresponding definition.
	pub def: ParamDef,
	/// The current value (the synchronization point between the node bridge and the OFX side).
	value: std::sync::Mutex<ParamValue>,
}

impl ParamInstance {
	/// Instantiate from a definition (called from the createInstance path). The instance props are a deep
	/// copy of the definition props (HS: SetInstance::makeParam's descriptor-property
	/// copy semantics — plugins read label/min/max etc. at instance time).
	pub fn from_def(def: ParamDef) -> Self {
		let value = def.default.clone();
		let props = def.props.clone();
		Self {
			props,
			def,
			value: std::sync::Mutex::new(value),
		}
	}

	/// Read the current value.
	pub fn get(&self) -> ParamValue {
		self.value.lock().unwrap_or_else(|e| e.into_inner()).clone()
	}

	/// Write the current value with OFX's paramSetValue semantics: replace only the value stored on the instance,
	/// bypassing the node bridge (no undo command, no host notification, no props change).
	///
	/// `value` replaces the old value wholesale, with no type/dimension validation or clamping — the caller (the
	/// param suite's set entry points) must build [`ParamValue`] according to the parameter's OFX type.
	/// The value sits behind a `Mutex`; if the lock is poisoned the inner value is recovered and writing continues,
	/// so this method returns no error and never fails because of lock poisoning.
	///
	/// Pairs with [`ParamInstance::get`] (read back verbatim); syncing the value back to the node is
	/// [`notify_instance_changed`] / the host-side `set_from_node`'s job,
	/// so a plugin calling this method does not automatically change the node input.
	pub fn set_ofx(&self, value: ParamValue) {
		*self.value.lock().unwrap_or_else(|e| e.into_inner()) = value;
	}

	/// The resident pointer of a string value (used by the param suite's get_string; points at the instance
	/// storage and is valid until the next set — a cloned CString pointer would dangle and must not be returned).
	pub(crate) fn string_ptr(&self) -> Option<*const std::ffi::c_char> {
		let v = self.value.lock().unwrap_or_else(|e| e.into_inner());
		match &*v {
			ParamValue::String(s) | ParamValue::StrChoice(s) => Some(s.as_ptr()),
			_ => None,
		}
	}

	/// Write from a quercusnode input value (node → plugin direction). The type mapping table mirrors
	/// C++'s `valueconvert`/`paraminstance.h` line by line (the `node_get` mirror):
	/// INT→Integer, FLOAT→Double, BOOL→Boolean, COMBO→Choice,
	/// COLOR→RGB/RGBA, VEC2/VEC3→Double(2D/3D)/Integer(2D/3D).
	/// Mismatched dimensions are truncated/zero-padded per OFX semantics (missing elements count as 0). The string family
	/// (QUERCUSNODE_VALUE_STRING)'s POD carries no data — this path does not change the value
	/// (it goes through the facade's string API, see `include/plugin/instance.h`);
	/// likewise for parametric (curve JSON text is written back via the render-time
	/// set_text_param injection in [`crate::node_factory`], see
	/// `crate::node_factory::execute_plugin_job`).
	/// A type mismatch → ignored (current value kept; C++ does not write back when `node_get` fails).
	pub fn set_from_node(&self, node_value: &crate::node::Value) {
		if let Some(pv) = param_value_from_node(node_value, &self.def.ofx_type) {
			self.set_ofx(pv);
		}
	}
}

/// Node value → ParamValue (interpreted by parameter type; the string family takes a dedicated path).
/// Mirrors `set_from_node`'s mapping table; the render driver's parameter overrides
/// (apply_param_overrides) reuse the same conversion.
pub(crate) fn param_value_from_node(
	v: &crate::node::Value,
	ofx_type: &str,
) -> Option<ParamValue> {
	use crate::node::node_value_type as T;
	match ofx_type {
		TYPE_DOUBLE => (v.r#type == T::FLOAT).then(|| ParamValue::Double([v.f[0], 0.0, 0.0], 1)),
		TYPE_DOUBLE2D => (v.r#type == T::VEC2).then(|| ParamValue::Double([v.f[0], v.f[1], 0.0], 2)),
		TYPE_DOUBLE3D => {
			(v.r#type == T::VEC3).then(|| ParamValue::Double([v.f[0], v.f[1], v.f[2]], 3))
		}
		TYPE_INTEGER => (v.r#type == T::INT).then_some(ParamValue::Int([v.num as i32, 0, 0], 1)),
		TYPE_INTEGER2D | TYPE_INTEGER3D => {
			let dim = if ofx_type == TYPE_INTEGER2D { 2 } else { 3 };
			Some(ParamValue::Int(
				[v.f[0] as i32, v.f[1] as i32, v.f[2] as i32],
				dim,
			))
		}
		TYPE_BOOLEAN => (v.r#type == T::BOOL).then_some(ParamValue::Bool(v.num != 0)),
		TYPE_CHOICE => (v.r#type == T::COMBO).then_some(ParamValue::Choice(v.num as i32)),
		TYPE_RGB => {
			(v.r#type == T::COLOR).then(|| ParamValue::Color([v.f[0], v.f[1], v.f[2], 0.0], 3))
		}
		TYPE_RGBA => (v.r#type == T::COLOR)
			.then(|| ParamValue::Color([v.f[0], v.f[1], v.f[2], v.f[3]], 4)),
		_ => None, // 字符串/无值类：POD 无数据，不改值
	}
}

/// ParamValue → [`crate::node::Value`] (plugin → node direction;
/// the string family goes through [`crate::node::set_input_string_undoable`]).
/// Mirrors the `value_int`/`value_double`/`value_vec`/`value_color` construction
/// in C++ `paraminstance.h`: an RGB colour gets alpha=1 (C++
/// `RGBInstance::set`'s `value_color(r,g,b,1.0)`). Value-less types and Bytes
/// have no node counterpart → None.
pub(crate) fn to_node_value(v: &ParamValue) -> Option<crate::node::Value> {
	use crate::node::node_value_type as T;
	let mut out = crate::node::Value::default();
	match v {
		ParamValue::Double(d, 1) => {
			out.r#type = T::FLOAT;
			out.f = [d[0], 0.0, 0.0, 0.0];
		}
		ParamValue::Double(d, 2) => {
			out.r#type = T::VEC2;
			out.f = [d[0], d[1], 0.0, 0.0];
		}
		ParamValue::Double(d, 3) => {
			out.r#type = T::VEC3;
			out.f = [d[0], d[1], d[2], 0.0];
		}
		ParamValue::Double(d, _) => {
			out.r#type = T::FLOAT;
			out.f = [d[0], 0.0, 0.0, 0.0];
		}
		ParamValue::Int(a, 1) => {
			out.r#type = T::INT;
			out.num = a[0] as i64;
		}
		ParamValue::Int(a, 2) => {
			out.r#type = T::VEC2;
			out.f = [a[0] as f64, a[1] as f64, 0.0, 0.0];
		}
		ParamValue::Int(a, 3) => {
			out.r#type = T::VEC3;
			out.f = [a[0] as f64, a[1] as f64, a[2] as f64, 0.0];
		}
		ParamValue::Bool(b) => {
			out.r#type = T::BOOL;
			out.num = *b as i64;
		}
		ParamValue::Choice(c) => {
			out.r#type = T::COMBO;
			out.num = *c as i64;
		}
		ParamValue::Color(c, 3) => {
			out.r#type = T::COLOR;
			out.f = [c[0], c[1], c[2], 1.0];
		}
		ParamValue::Color(c, 4) => {
			out.r#type = T::COLOR;
			out.f = *c;
		}
		_ => return None,
	}
	Some(out)
}

/// A set of parameter instances (instance level).
pub struct ParamSetInstance {
	/// All parameters (stable definition order; Box keeps every handle address stable).
	pub params: Vec<Box<ParamInstance>>,
}

impl ParamSetInstance {
	/// Find a parameter in the instance set by parameter name.
	///
	/// `name` is compared exactly (case-sensitive) against [`ParamDef::name`]
	/// (the parameter identifier the plugin gives via `OfxParamPropName` at describe
	/// time); the first match is returned as a borrowed reference bounded by `&self`. `ParamSetInstance::params` is in definition order,
	/// so the hit order matches the describe order. Returns `None` when not found
	/// (the normal path when a plugin queries an unknown parameter name, not an error).
	pub fn find(&self, name: &str) -> Option<&ParamInstance> {
		self.params
			.iter()
			.find(|p| p.def.name == name)
			.map(|b| b.as_ref())
	}

	/// A snapshot (used by the introspection C ABI and by snapshot tests).
	///
	/// The signature used to be `Vec<(&str, &ParamValue)>`: the values sit behind a Mutex, so no live
	/// reference can be returned — changed to owned `(String, ParamValue)` (introspection semantics unchanged).
	pub fn snapshot(&self) -> Vec<(String, ParamValue)> {
		self.params
			.iter()
			.map(|p| (p.def.name.clone(), p.get()))
			.collect()
	}
}

/// The write-back entry point for the plugin → node direction (triggered by the instanceChanged action).
/// Locates the bound node through [`crate::node`] (the identity registry), then wraps the change
/// as an undoable modification via `quercus_undo::undocommand::UndoCommand`. With no bound
/// node or no identity match it is a no-op (after the single-lib unification the identity registry is rebuilt as
/// [`crate::node::register_node`]/[`crate::node::node_from_identity`];
/// registered during facade setup, weak entries expire naturally once the project is released).
///
/// Semantics follow C++ `paraminstance.h`'s `set()` path (`detail::node_set`/
/// `node_set_at`):
/// - Only [`ChangeReason::PluginEdited`] (plugin self-edit) is written back. UserEdited/
///   TimeChanged are host-side changes whose values were already synced by [`ParamInstance::set_from_node`],
///   so they are not written back again;
/// - The string family goes through [`crate::node::set_input_string_undoable`]
///   (the POD carries no string data); parametric curves likewise go through that string setter
///   and are written back as JSON text ([`crate::param_curve::curves_to_json`],
///   with the same undo semantics as scalar write-back — the plugin edits a curve on the instance → the node input updates,
///   and the project serialization keeps it with the standard values);
/// - Value-less types (PushButton/Group/Page) and Bytes have no node counterpart → no-op;
/// - Inside an edit transaction ([`crate::instance::Instance::in_edit`]) the change joins the multi
///   command; otherwise a single command takes effect with an immediate redo (C++ `submit_undo_command`).
pub(crate) fn notify_instance_changed(
	instance: &crate::instance::Instance,
	param_name: &str,
	reason: ChangeReason,
) {
	use crate::node;
	use std::sync::atomic::Ordering;

	if !matches!(reason, ChangeReason::PluginEdited) {
		return;
	}
	// 未绑定节点 → no-op（C++：!node_.ctx 时 set 只写本地值）。
	let node_id = instance.node_identity.load(Ordering::Relaxed);
	if node_id == 0 {
		return;
	}
	// 身份查无（未登记 / project 已释放）→ no-op。
	let Some(node_ref) = node::node_from_identity(node_id) else {
		return;
	};
	let Some(param) = instance.params.find(param_name) else {
		return;
	};
	let value = param.get();
	let label = format!("Change {param_name}");
	match to_node_value(&value) {
		Some(nv) => {
			if let Ok(cmd) = node::set_input_undoable(&node_ref, param_name, &nv) {
				instance.submit_undo_command(cmd, &label);
			}
		}
		None => match &value {
			ParamValue::String(s) | ParamValue::StrChoice(s) => {
				let Ok(s) = s.to_str() else {
					return;
				};
				if let Ok(cmd) = node::set_input_string_undoable(&node_ref, param_name, s) {
					instance.submit_undo_command(cmd, &label);
				}
			}
			// parametric：曲线序列化为 JSON 文本写回输入（节点输入是
			// Parametric 声明类型的 Text 值；经字符串 setter 存储）。
			ParamValue::Parametric(curves) => {
				let json = crate::param_curve::curves_to_json(curves);
				if let Ok(cmd) = node::set_input_string_undoable(&node_ref, param_name, &json) {
					instance.submit_undo_command(cmd, &label);
				}
			}
			_ => {} // 无值类 / Bytes：无节点对应
		},
	}
}

/// The instanceChanged reason (OFX kOfxChange*).
#[derive(Clone, Copy, Debug)]
pub enum ChangeReason {
	/// User edit.
	UserEdited,
	/// Plugin self-edit.
	PluginEdited,
	/// Time change.
	TimeChanged,
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::node::node_value_type as T;

	fn param(ofx_type: &str) -> ParamInstance {
		ParamInstance::from_def(ParamDef::new("p", ofx_type))
	}

	/// The ParamValue → quercusnode Value type mapping table (mirrors C++
	/// paraminstance.h's value_* construction).
	#[test]
	fn to_node_value_mapping() {
		use crate::node::Value;
		let c = |t: i32| Value {
			r#type: t,
			num: 0,
			den: 0,
			f: [0.0; 4],
		};
		// Double 族。
		let v = to_node_value(&ParamValue::Double([1.5, 0.0, 0.0], 1)).unwrap();
		assert_eq!(v, Value::float(1.5));
		let v = to_node_value(&ParamValue::Double([1.0, 2.0, 0.0], 2)).unwrap();
		assert_eq!(v, Value::vec(&[1.0, 2.0]));
		let v = to_node_value(&ParamValue::Double([1.0, 2.0, 3.0], 3)).unwrap();
		assert_eq!(v, Value::vec(&[1.0, 2.0, 3.0]));
		// Int 族（2D/3D 升格为 VEC2/VEC3 浮点载荷）。
		let v = to_node_value(&ParamValue::Int([7, 0, 0], 1)).unwrap();
		assert_eq!(v, Value::int(7));
		let v = to_node_value(&ParamValue::Int([1, 2, 0], 2)).unwrap();
		assert_eq!(v, Value::vec(&[1.0, 2.0]));
		// Bool / Choice。
		assert_eq!(
			to_node_value(&ParamValue::Bool(true)).unwrap(),
			Value::bool_(true)
		);
		assert_eq!(
			to_node_value(&ParamValue::Choice(3)).unwrap(),
			Value::combo(3)
		);
		// Color：RGB 补 alpha=1（C++ oracle），RGBA 全透传。
		let v = to_node_value(&ParamValue::Color([0.1, 0.2, 0.3, 0.0], 3)).unwrap();
		assert_eq!(v, Value::color(0.1, 0.2, 0.3, 1.0));
		let v = to_node_value(&ParamValue::Color([0.1, 0.2, 0.3, 0.9], 4)).unwrap();
		assert_eq!(v, Value::color(0.1, 0.2, 0.3, 0.9));
		// 无值类 / Bytes → None（无节点对应）。
		assert_eq!(to_node_value(&ParamValue::PushButton), None);
		assert_eq!(to_node_value(&ParamValue::Container), None);
		assert_eq!(to_node_value(&ParamValue::Bytes(vec![1])), None);
		// 字符串族走专用桥（POD 路径 None）。
		assert_eq!(to_node_value(&ParamValue::String(cs(""))), None);
		let _ = c(T::NONE); // 哨兵：类型常量可达
	}

	/// type_default's value-less/bytes-family baseline (the fallback before paramDefine rejects).
	#[test]
	fn type_default_edge_cases() {
		assert!(matches!(type_default(TYPE_BYTES), ParamValue::Bytes(_)));
		assert!(matches!(type_default(TYPE_CUSTOM), ParamValue::Bytes(_)));
		assert!(matches!(
			type_default(TYPE_PUSHBUTTON),
			ParamValue::PushButton
		));
		assert!(matches!(type_default(TYPE_GROUP), ParamValue::Container));
		assert!(matches!(type_default(TYPE_PAGE), ParamValue::Container));
		// 未知类型占位（paramDefine 已拒绝，此处兜底）。
		assert!(matches!(
			type_default("OfxParamTypeBogus"),
			ParamValue::Container
		));
		// 数值族的默认 0。
		assert!(matches!(
			type_default(TYPE_INTEGER),
			ParamValue::Int([0, 0, 0], 1)
		));
		assert!(matches!(
			type_default(TYPE_DOUBLE2D),
			ParamValue::Double([0.0, 0.0, 0.0], 2)
		));
		assert!(matches!(
			type_default(TYPE_RGBA),
			ParamValue::Color([0.0, 0.0, 0.0, 0.0], 4)
		));
	}

	/// parametric's default: 1 identity curve; the 4 dedicated properties in place
	/// (dimension=1, range=(0,1), UIColour unset, interact background NULL).
	#[test]
	fn parametric_type_default() {
		assert!(matches!(
			type_default(TYPE_PARAMETRIC),
			ParamValue::Parametric(ref c) if c.len() == 1 && c[0].len() == 2
		));
		let d = ParamDef::new("curve", TYPE_PARAMETRIC);
		assert!(matches!(d.default, ParamValue::Parametric(ref c) if c.len() == 1));
		assert!(matches!(
			d.props.get(P_PARAMETRIC_DIMENSION, 0),
			Some(Value::Int(1))
		));
		assert!(matches!(
			d.props.get(P_PARAMETRIC_RANGE, 0),
			Some(Value::Double(0.0))
		));
		assert!(matches!(
			d.props.get(P_PARAMETRIC_RANGE, 1),
			Some(Value::Double(1.0))
		));
		// UIColour 默认未设：属性不存在（插件首写隐式创建），而不是
		// 预定义的空数组（空数组会把 index >= 1 的写入挡成 BadIndex）。
		assert!(d.props.get(P_PARAMETRIC_UI_COLOUR, 0).is_none());
		assert!(matches!(
			d.props.get(P_PARAMETRIC_INTERACT_BG, 0),
			Some(Value::Pointer(p)) if p.is_null()
		));
		// 默认曲线求值为恒等。
		if let ParamValue::Parametric(curves) = &d.default {
			assert_eq!(curves[0].evaluate(0.5), 0.5);
		}
	}

	/// set_from_node's dimension truncation/zero-padding and the silent string path.
	#[test]
	fn set_from_node_dimension_rules() {
		// Integer2D 缺第三维补零；浮点截断。
		let p = param(TYPE_INTEGER2D);
		p.set_from_node(&crate::node::Value::vec(&[1.9, 2.9]));
		assert_eq!(p.get(), ParamValue::Int([1, 2, 0], 2));
		// StrChoice 参数遇 STRING 类型：POD 无数据 → 不改值。
		let p = param(TYPE_STRCHOICE);
		p.set_from_node(&crate::node::Value::string());
		assert!(matches!(p.get(), ParamValue::StrChoice(_)));
		// Bytes/Custom 参数：任意节点值都不改（无映射）。
		let p = param(TYPE_CUSTOM);
		p.set_from_node(&crate::node::Value::float(3.0));
		assert!(matches!(p.get(), ParamValue::Bytes(_)));
	}
}
