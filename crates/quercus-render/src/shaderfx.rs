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

//! Effect shader translation: the nodes' embedded GLSL fragment shaders
//! (the C++ `:/shaders/*.frag` corpus, kept verbatim in quercus-node) are
//! converted to WGSL at runtime through naga and run as wgpu fullscreen
//! passes.
//!
//! The conversion mirrors the C++ Vulkan backend's mechanical rewrite
//! (`vulkanrenderer.cpp` `ConvertGlslToVulkan` + `ExtractUniforms`):
//!
//! - a `#version 450 core` prelude is prepended;
//! - legacy `texture2D(`/`texture3D(` calls are renamed to `texture(`;
//! - the pipeline I/O globals get explicit locations
//!   (`layout(location = 0) in vec2 ove_texcoord;`,
//!   `layout(location = 0) out vec4 frag_color;`);
//! - loose `uniform <type> <name>;` declarations are extracted: samplers
//!   get explicit `set`/`binding` qualifiers, and value uniforms are
//!   collected into one anonymous `std140` uniform block (GLSL 450
//!   anonymous block members stay accessible by their bare names, so the
//!   shader body needs no rewriting).
//!
//! Uniform values are packed by the caller following std140 rules
//! (float/int/bool 4/4, vec2 8/8, vec3 12/16, vec4 16/16, mat4 64/16 —
//! the same table the C++ `GetStd140Size/Alignment` used).

use crate::error::{Error, Result};

/// A value uniform's GLSL type (std140 packing + `NodeValue` mapping).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UniformType {
	/// `float`.
	Float,
	/// `int`.
	Int,
	/// `bool` (stored as `int` in the block; WGSL has no shareable bool).
	Bool,
	/// `vec2`.
	Vec2,
	/// `vec3`.
	Vec3,
	/// `vec4`.
	Vec4,
	/// `mat4`.
	Mat4,
	/// `vec4[N]` (an array uniform; the polygon generator's bezier point
	/// table is the only consumer, indexed by `[i]` in the shader body).
	Vec4Array(usize),
}

impl UniformType {
	/// The GLSL type keyword, or `None` when it is not a value uniform
	/// (samplers, arrays and unknown types are not packable).
	fn from_keyword(kw: &str) -> Option<UniformType> {
		Some(match kw {
			"float" => UniformType::Float,
			"int" => UniformType::Int,
			"bool" => UniformType::Bool,
			"vec2" => UniformType::Vec2,
			"vec3" => UniformType::Vec3,
			"vec4" => UniformType::Vec4,
			"mat4" => UniformType::Mat4,
			_ => return None,
		})
	}

	/// The GLSL keyword back (block re-emission). Array types have their
	/// count appended (`vec4{name}[{count}]`), matching C++ uniform-array
	/// declarations.
	fn keyword(self) -> &'static str {
		match self {
			UniformType::Float => "float",
			UniformType::Int => "int",
			UniformType::Bool => "bool",
			UniformType::Vec2 => "vec2",
			UniformType::Vec3 => "vec3",
			UniformType::Vec4 => "vec4",
			UniformType::Mat4 => "mat4",
			UniformType::Vec4Array(_) => "vec4",
		}
	}

	/// std140 base alignment in bytes (C++ `GetStd140Alignment`).
	pub fn align(self) -> usize {
		match self {
			UniformType::Float | UniformType::Int | UniformType::Bool => 4,
			UniformType::Vec2 => 8,
			UniformType::Vec3 | UniformType::Vec4 | UniformType::Mat4 => 16,
			UniformType::Vec4Array(_) => 16,
		}
	}

	/// std140 storage size in bytes (C++ `GetStd140Size`).
	pub fn size(self) -> usize {
		match self {
			UniformType::Float | UniformType::Int | UniformType::Bool => 4,
			UniformType::Vec2 => 8,
			UniformType::Vec3 => 12,
			UniformType::Vec4 => 16,
			UniformType::Mat4 => 64,
			UniformType::Vec4Array(n) => 16 * n.max(1),
		}
	}
}

/// A value uniform (std140-packed into the uniform block).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UniformDecl {
	/// Uniform name (= the node input id, the Olive convention).
	pub name: String,
	/// Its type.
	pub ty: UniformType,
	/// Byte offset in the packed block (assigned by [`translate`]).
	pub offset: usize,
}

/// The result of translating one effect fragment shader.
#[derive(Clone, Debug)]
pub struct TranslatedShader {
	/// The WGSL fragment module (entry point `main`).
	pub wgsl: String,
	/// Value uniforms in block order (offsets assigned, block tail-padded
	/// to 16).
	pub uniforms: Vec<UniformDecl>,
	/// The packed uniform block size in bytes (0 = no value uniforms).
	pub uniform_block_bytes: usize,
	/// Texture input names in binding order (combined `sampler2D` etc.;
	/// the first one is the effect's main input by Olive convention).
	pub textures: Vec<String>,
	/// Fragment input varyings in location order (`ove_texcoord` first,
	/// then any effect-specific varyings like cornerpin's perspective
	/// helpers). The runner's vertex stage must produce all of them.
	pub varyings: Vec<String>,
}

// ---------------------------------------------------------------------------
// The effect runner
// ---------------------------------------------------------------------------

/// A compiled effect: the translated shader plus its cached pipeline.
pub struct CompiledEffect {
	/// The translation result (uniform layout + texture bindings).
	pub translated: TranslatedShader,
	/// The compiled pipeline in the context cache.
	pub program: std::sync::Arc<quercus_core::backend::ShaderProgram>,
}

/// Translate `glsl` and compile the pipeline on `ctx`. `key` is the
/// pipeline cache key (the effect type id plus any shader-variant id).
pub fn compile_effect(
	ctx: &quercus_core::backend::GpuContext,
	key: &str,
	glsl: &str,
	filtering: bool,
) -> Result<CompiledEffect> {
	let translated = translate(glsl)?;
	let program = ctx.compile_shader_pass(
		key,
		&translated.wgsl,
		translated.textures.len() as u32,
		!translated.uniforms.is_empty(),
		filtering,
	)?;
	Ok(CompiledEffect {
		translated,
		program,
	})
}

/// Run an effect: shade `dst` from the input textures with `params` as
/// the uniform values.
///
/// - `inputs` maps the shader's texture names to context texture tokens;
///   declared textures without an input bind the context's 1×1
///   placeholder and get `<name>_enabled = 0` (C++ Blit's texture
///   binding + enable-flag convention). The first declared texture is
///   the main input — and the iterative one when `iterations` > 1
///   (C++ `ShaderJob::SetIterations` with `tex_in`).
/// - `iterations` runs the shader that many times, feeding each pass's
///   output back as the main input (C++ `OpenGLRenderer::Blit`'s
///   ping-pong; the `ove_iteration` uniform tracks the pass index).
///   `iterative_input` (C++ `ShaderJob::iterative_input`) names the
///   texture the feedback lands in — e.g. the drop shadow's
///   `previous_iteration_in` — while the other samplers keep their
///   original bindings; empty/`None` feeds back into the main input.
/// - Well-known uniforms are auto-filled when declared but absent from
///   `params`: `resolution_in` (the frame size), `ove_iteration`,
///   `ove_mvpmat` (identity).
pub fn run_effect(
	ctx: &quercus_core::backend::GpuContext,
	effect: &CompiledEffect,
	params: &quercus_node::value::NodeValueRow,
	inputs: &[(String, u64)],
	dst: u64,
	size: (i32, i32),
	iterations: u32,
	iterative_input: Option<&str>,
) -> Result<()> {
	use quercus_node::value::NodeValue;

	let declares = |name: &str| effect.translated.uniforms.iter().any(|u| u.name == name);

	// Resolve every declared texture to a token (placeholder when the
	// effect's input is unconnected).
	let mut tokens: Vec<u64> = Vec::with_capacity(effect.translated.textures.len());
	let mut row = params.clone();
	for (i, name) in effect.translated.textures.iter().enumerate() {
		let token = inputs
			.iter()
			.find(|(n, _)| n == name)
			.map(|(_, t)| *t)
			.or_else(|| {
				if i == 0 {
					inputs.first().map(|(_, t)| *t)
				} else {
					None
				}
			});
		match token {
			Some(t) => {
				tokens.push(t);
				let flag = format!("{name}_enabled");
				if declares(&flag) && !row.contains_key(&flag) {
					row.insert(flag, NodeValue::Boolean(true));
				}
			}
			None => tokens.push(ctx.placeholder_texture()?),
		}
	}

	// Well-known uniforms (C++ inserts resolution_in at job-build time;
	// ove_mvpmat defaults to identity in Blit).
	if declares("resolution_in") && !row.contains_key("resolution_in") {
		row.insert(
			"resolution_in".to_string(),
			NodeValue::Vec2([size.0 as f64, size.1 as f64]),
		);
	}
	if declares("ove_mvpmat") && !row.contains_key("ove_mvpmat") {
		let mut m = [0.0f64; 16];
		for i in 0..4 {
			m[i * 4 + i] = 1.0;
		}
		row.insert("ove_mvpmat".to_string(), NodeValue::Matrix(m));
	}

	let iterations = iterations.max(1);
	if iterations == 1 {
		let uniforms = pack_uniforms(&effect.translated, &row);
		return ctx.run_shader_pass(&effect.program, &uniforms, &tokens, dst);
	}

	// Ping-pong (C++ Blit): one scratch texture for two passes, two for
	// longer chains; the last pass always lands in `dst`. Each pass feeds
	// back into the iterative input (C++ `ShaderJob::iterative_input`),
	// defaulting to the first (main) texture.
	let feedback = iterative_input
		.and_then(|name| effect.translated.textures.iter().position(|t| t == name))
		.unwrap_or(0);
	let scratch_a = ctx.create_texture(size.0, size.1)?;
	let scratch_b = if iterations > 2 {
		Some(ctx.create_texture(size.0, size.1)?)
	} else {
		None
	};
	let result = (|| -> Result<()> {
		let mut input_tokens = tokens.clone();
		for i in 0..iterations {
			let mut pass_row = row.clone();
			if declares("ove_iteration") {
				pass_row.insert("ove_iteration".to_string(), NodeValue::Int(i as i64));
			}
			let target = if i == iterations - 1 {
				dst
			} else if i % 2 == 0 {
				scratch_a
			} else {
				scratch_b.unwrap()
			};
			let uniforms = pack_uniforms(&effect.translated, &pass_row);
			ctx.run_shader_pass(&effect.program, &uniforms, &input_tokens, target)?;
			if !input_tokens.is_empty() {
				let slot = feedback.min(input_tokens.len() - 1);
				input_tokens[slot] = target;
			}
		}
		Ok(())
	})();
	ctx.destroy_texture(scratch_a);
	if let Some(b) = scratch_b {
		ctx.destroy_texture(b);
	}
	result
}

/// Replace whole-word occurrences of `name` in `s` with `replacement`
/// (identifier boundaries: alphanumerics and `_`). The node-shader
/// corpus uses plain identifiers, so this simple scan suffices — no
/// regex dependency.
fn replace_ident(s: &str, name: &str, replacement: &str) -> String {
	fn is_ident_char(c: u8) -> bool {
		c.is_ascii_alphanumeric() || c == b'_'
	}
	let bytes = s.as_bytes();
	let name = name.as_bytes();
	let mut out = String::with_capacity(s.len());
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i..].starts_with(name)
			&& (i == 0 || !is_ident_char(bytes[i - 1]))
			&& !bytes.get(i + name.len()).is_some_and(|&c| is_ident_char(c))
		{
			out.push_str(replacement);
			i += name.len();
		} else {
			out.push(bytes[i] as char);
			i += 1;
		}
	}
	out
}

/// Parse a standalone `in`/`out` varying declaration line body (after
/// the direction keyword), e.g. `vec2 ove_texcoord;` → `(name, type)`.
/// Function parameter lists and anything else are rejected.
fn parse_plain_global(rest: &str) -> Option<(String, String)> {
	let rest = rest.trim().strip_suffix(';')?.trim();
	let (ty, name) = rest.split_once(char::is_whitespace)?;
	let name = name.trim();
	if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
		return None;
	}
	Some((name.to_string(), ty.to_string()))
}

/// Binding 0 is the uniform block; textures/samplers follow.
const UNIFORM_BLOCK_BINDING: u32 = 0;

/// Pack the uniform block for `shader` from `params` (the effect's
/// parameter row; uniform names are the node input ids by Olive
/// convention). Packing follows the declared uniform types, converting
/// from whatever `NodeValue` shape arrived (the C++ Blit dispatched on
/// the value type with GL's implicit conversions; here the declared
/// type wins). Undeclared params are skipped; missing values stay zero.
pub fn pack_uniforms(
	shader: &TranslatedShader,
	params: &quercus_node::value::NodeValueRow,
) -> Vec<u8> {
	use quercus_node::value::NodeValue;
	let mut buf = vec![0u8; shader.uniform_block_bytes];
	for decl in &shader.uniforms {
		let Some(value) = params.get(&decl.name) else {
			continue;
		};
		let f32s: Vec<f32> = match (decl.ty, value) {
			(UniformType::Float, NodeValue::Float(v)) => vec![*v as f32],
			(UniformType::Float, NodeValue::Int(v) | NodeValue::Combo(v)) => vec![*v as f32],
			(UniformType::Float, NodeValue::Boolean(v)) => vec![f32::from(u8::from(*v))],
			(UniformType::Int, NodeValue::Int(v) | NodeValue::Combo(v)) => {
				write_i32(&mut buf, decl.offset, *v as i32);
				continue;
			}
			(UniformType::Int, NodeValue::Float(v)) => {
				write_i32(&mut buf, decl.offset, *v as i32);
				continue;
			}
			(UniformType::Bool, NodeValue::Boolean(v)) => {
				write_i32(&mut buf, decl.offset, i32::from(*v));
				continue;
			}
			(UniformType::Bool, NodeValue::Int(v) | NodeValue::Combo(v)) => {
				write_i32(&mut buf, decl.offset, i32::from(*v != 0));
				continue;
			}
			(UniformType::Vec2, NodeValue::Vec2(v)) => v.iter().map(|x| *x as f32).collect(),
			(UniformType::Vec3, NodeValue::Vec3(v)) => v.iter().map(|x| *x as f32).collect(),
			// A color packs into a vec3 slot as its RGB (C++ Color →
			// glUniform4f only for vec4; a vec3 target takes rgb).
			(UniformType::Vec3, NodeValue::Color(v)) => v[..3].iter().map(|x| *x as f32).collect(),
			(UniformType::Vec4, NodeValue::Vec4(v) | NodeValue::Color(v)) => {
				v.iter().map(|x| *x as f32).collect()
			}
			(UniformType::Vec4Array(n), NodeValue::Vec4Array(v)) => {
				let mut out = Vec::with_capacity(n * 4);
				for el in v.iter().take(n) {
					out.extend(el.iter().map(|x| *x as f32));
				}
				// Pad to the declared count (each element is 16 bytes; the
				// buffer is sized n*16 regardless).
				let have = out.len();
				out.resize((n * 4).max(have), 0.0f32);
				out
			}
			// Matrices: GLSL mat4 is column-major; the NodeValue comment
			// marks the layout row-major, so transpose on the way in.
			(UniformType::Mat4, NodeValue::Matrix(m)) => {
				let mut cols = Vec::with_capacity(16);
				for c in 0..4 {
					for r in 0..4 {
						cols.push(m[r * 4 + c] as f32);
					}
				}
				cols
			}
			_ => continue,
		};
		for (i, v) in f32s.iter().enumerate() {
			let at = decl.offset + i * 4;
			if at + 4 <= buf.len() {
				buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
			}
		}
	}
	buf
}

/// Write `v` as little-endian bytes into `buf` at `offset`, if it fits.
///
/// `buf` is the uniform byte buffer, `offset` a byte position inside it and
/// `v` the integer value (e.g. an int uniform); a write that would run past
/// `buf.len()` is silently skipped rather than panicking, so a short buffer
/// leaves the tail untouched.
fn write_i32(buf: &mut [u8], offset: usize, v: i32) {
	if offset + 4 <= buf.len() {
		buf[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
	}
}

/// Whether a GLSL type keyword is a combined sampler (C++
/// `IsSamplerType`: sampler\*D / samplerCube / sampler2DArray).
fn is_sampler_type(kw: &str) -> bool {
	kw.starts_with("sampler")
}

/// Parse a `uniform <type> <name>;` declaration line (the constrained
/// style of the node shader corpus: one declaration per line, no layout
/// qualifiers, no initializers). Arrays ARE supported:
/// `uniform <type> <name>[<count>];` (the polygon generator's point
/// table). `None` for any other line (samplers are handled by the
/// caller). Returns `(base keyword, name, array count)`.
fn parse_uniform_line(line: &str) -> Option<(&str, &str, usize)> {
	let t = line.trim_start();
	let rest = t.strip_prefix("uniform")?;
	if !rest.starts_with(char::is_whitespace) {
		return None;
	}
	let rest = rest.trim_start();
	let (ty, rest) = rest.split_once(char::is_whitespace)?;
	let rest = rest.trim_start().trim_end_matches(';');
	let (name, count) = if let Some(idx) = rest.find('[') {
		let name = rest[..idx].trim();
		let end = rest[idx..].find(']')?;
		let count: usize = rest[idx + 1..idx + end].trim().parse().ok()?;
		(name, count.max(1))
	} else {
		(rest.trim(), 1)
	};
	if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
		return None;
	}
	Some((ty, name, count))
}

/// Translate one GLSL fragment shader to WGSL (naga glsl-in → wgsl-out).
/// The source keeps the Olive node-shader conventions; see the module
/// docs for the rewrite steps.
pub fn translate(glsl: &str) -> Result<TranslatedShader> {
	let mut body_lines: Vec<String> = Vec::new();
	let mut uniforms: Vec<(UniformType, String, usize)> = Vec::new();
	let mut textures: Vec<String> = Vec::new();

	for line in glsl.lines() {
		if let Some((ty, name, count)) = parse_uniform_line(line) {
			if is_sampler_type(ty) {
				for _ in 0..count {
					textures.push(name.to_string());
				}
				continue;
			}
			let base = UniformType::from_keyword(ty);
			let uniform = match (base, count) {
				(Some(UniformType::Vec4), n) if n > 1 => UniformType::Vec4Array(n),
				(Some(t), n) if n == 1 => t,
				_ => {
					return Err(Error::Failed(format!(
						"unsupported array uniform type in shader: {ty} {name}[{count}]"
					)));
				}
			};
			uniforms.push((uniform, name.to_string(), count));
			continue;
		}
		body_lines.push(line.to_string());
	}

	let mut src = String::from("#version 450 core\n");
	let mut in_loc = 0u32;
	let mut out_loc = 0u32;
	let mut varyings: Vec<String> = Vec::new();

	// Re-emit the extracted uniforms BEFORE the body (GLSL requires
	// declarations to precede use): the value block first (binding 0),
	// then the samplers. The block is anonymous — GLSL 450 anonymous
	// block members stay accessible by their bare names, so the shader
	// body needs no rewriting.
	if !uniforms.is_empty() {
		src.push_str("layout(std140, set = 0, binding = ");
		src.push_str(&UNIFORM_BLOCK_BINDING.to_string());
		src.push_str(") uniform QuercusParams {\n");
		for (ty, name, count) in &uniforms {
			// bools are declared as int (WGSL has no host-shareable
			// bool); the body's uses were rewritten to `bool(x)`.
			let kw = if *ty == UniformType::Bool {
				"int"
			} else {
				ty.keyword()
			};
			if *count > 1 {
				src.push_str(&format!("    {kw} {name}[{}];\n", count));
			} else {
				src.push_str(&format!("    {kw} {name};\n"));
			}
		}
		src.push_str("};\n");
	}
	for (i, name) in textures.iter().enumerate() {
		// Split the combined sampler2D: texture at an odd binding, its
		// sampler right after (the binding map is reported through
		// [`TranslatedShader::textures`] in declaration order).
		src.push_str(&format!(
			"layout(set = 0, binding = {}) uniform texture2D {};\n\
			 layout(set = 0, binding = {}) uniform sampler {}_s;\n",
			1 + 2 * i,
			name,
			2 + 2 * i,
			name
		));
	}

	for line in &body_lines {
		let mut l = line.clone();
		// Strip a pre-existing #version (the prelude pins 450 core).
		if l.trim_start().starts_with("#version") {
			continue;
		}
		// Legacy sampling entry points.
		l = l.replace("texture2D(", "texture(");
		l = l.replace("texture3D(", "texture(");
		l = l.replace("textureCube(", "texture(");
		// naga's GLSL frontend has no combined sampler2D uniforms: split
		// each into (texture2D, sampler) and combine at the call site
		// (`texture(sampler2D(tex, tex_s), uv)` — the same style naga's
		// own GLSL tests use).
		for name in &textures {
			l = l.replace(
				&format!("texture({name},"),
				&format!("texture(sampler2D({name}, {name}_s),"),
			);
		}
		// WGSL has no host-shareable bool: bool uniforms live in the
		// block as `int`, so their uses become `bool(x)` (nonzero test).
		for (ty, name, _count) in &uniforms {
			if *ty == UniformType::Bool {
				l = replace_ident(&l, name, &format!("bool({name})"));
			}
		}
		// Explicit interface locations (C++ ConvertGlslToVulkan did this
		// for the two known globals; shaders with extra varyings — e.g.
		// cornerpin's perspective helpers — get sequential locations so
		// nothing collides at location 0).
		let trimmed = l.trim_start();
		if let Some(rest) = trimmed.strip_prefix("in ") {
			if let Some((name, _ty)) = parse_plain_global(rest) {
				let indent = &l[..l.len() - trimmed.len()];
				l = format!("{indent}layout(location = {in_loc}) in {}", rest.trim());
				varyings.push(name);
				in_loc += 1;
			}
		} else if let Some(rest) = trimmed.strip_prefix("out ") {
			if parse_plain_global(rest).is_some() {
				let indent = &l[..l.len() - trimmed.len()];
				l = format!("{indent}layout(location = {out_loc}) out {}", rest.trim());
				out_loc += 1;
			}
		}
		src.push_str(&l);
		src.push('\n');
	}

	let module = naga::front::glsl::Frontend::default()
		.parse(
			&naga::front::glsl::Options::from(naga::ShaderStage::Fragment),
			&src,
		)
		.map_err(|e| Error::Failed(format!("GLSL parse failed: {e:?}")))?;
	let info = naga::valid::Validator::new(
		naga::valid::ValidationFlags::all(),
		naga::valid::Capabilities::all(),
	)
	.validate(&module)
	.map_err(|e| Error::Failed(format!("translated shader failed validation: {e:?}")))?;
	let wgsl =
		naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
			.map_err(|e| Error::Failed(format!("WGSL emission failed: {e:?}")))?;

	// std140 offsets (declaration order; the block tail pads to 16).
	let mut offset = 0usize;
	let mut decls = Vec::with_capacity(uniforms.len());
	for (ty, name, _count) in uniforms {
		let align = ty.align();
		offset = offset.next_multiple_of(align);
		decls.push(UniformDecl { name, ty, offset });
		offset += ty.size();
	}
	let uniform_block_bytes = if decls.is_empty() {
		0
	} else {
		offset.next_multiple_of(16)
	};

	Ok(TranslatedShader {
		wgsl,
		uniforms: decls,
		uniform_block_bytes,
		textures,
		varyings,
	})
}

#[cfg(test)]
mod tests;
