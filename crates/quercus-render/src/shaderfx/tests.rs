use super::*;

/// The minimal node-shader shape: texture input + one float uniform.
#[test]
fn translates_minimal_effect_shader() {
	let glsl = r#"
uniform sampler2D tex_in;
uniform float gain_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
    frag_color = texture(tex_in, ove_texcoord) * gain_in;
}
"#;
	let out = translate(glsl).expect("translate");
	assert_eq!(out.textures, vec!["tex_in"]);
	assert_eq!(out.uniforms.len(), 1);
	assert_eq!(out.uniforms[0].name, "gain_in");
	assert_eq!(out.uniforms[0].ty, UniformType::Float);
	assert_eq!(out.uniforms[0].offset, 0);
	assert_eq!(out.uniform_block_bytes, 16);
	assert!(
		out.wgsl.contains("fn main"),
		"WGSL entry point: {}",
		out.wgsl
	);
}

/// bool/int/vec/color-shaped uniforms pack with std140 offsets.
#[test]
fn std140_offsets_match_the_cpp_table() {
	let glsl = r#"
uniform sampler2D tex_in;
uniform bool flag_in;
uniform vec2 center_in;
uniform float radius_in;
uniform vec4 color_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
    vec4 c = texture(tex_in, ove_texcoord);
    frag_color = flag_in ? color_in * radius_in : vec4(c.xy + center_in, c.zw);
}
"#;
	let out = translate(glsl).expect("translate");
	let offsets: Vec<(&str, usize)> = out
		.uniforms
		.iter()
		.map(|u| (u.name.as_str(), u.offset))
		.collect();
	// bool 4/4 @0; vec2 align 8 @8; float 4 @16; vec4 align 16 @32.
	assert_eq!(
		offsets,
		vec![
			("flag_in", 0),
			("center_in", 8),
			("radius_in", 16),
			("color_in", 32)
		]
	);
	assert_eq!(out.uniform_block_bytes, 48);
}

/// Array uniforms translate into the block as packed std140 arrays
/// (polygon's `points_in[64]`; the declaration order determines the
/// offsets: `int` @0, the vec4 array aligned to 16 @16, the trailing
/// vec2 aligned to 8).
#[test]
fn array_uniforms_translate_and_pack() {
	use quercus_node::value::{NodeValue, NodeValueRow};
	let glsl = r#"
uniform int point_count;
uniform vec4 points_in[64];
uniform vec2 resolution_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() { frag_color = vec4(0.0); }
"#;
	let out = translate(glsl).expect("translate array uniforms");
	let decls: Vec<(&str, &UniformType, usize)> = out
		.uniforms
		.iter()
		.map(|u| (u.name.as_str(), &u.ty, u.offset))
		.collect();
	assert_eq!(
		decls,
		vec![
			("point_count", &UniformType::Int, 0),
			("points_in", &UniformType::Vec4Array(64), 16),
			("resolution_in", &UniformType::Vec2, 16 + 64 * 16),
		]
	);
	assert_eq!(
		out.uniform_block_bytes,
		(16 + 64 * 16 + 8usize).next_multiple_of(16)
	);

	let mut row = NodeValueRow::new();
	row.insert("point_count".into(), NodeValue::Int(3));
	row.insert(
		"points_in".into(),
		NodeValue::Vec4Array(vec![[1.0, 2.0, 0.0, 0.0], [3.0, 4.0, 0.0, 0.0]]),
	);
	let buf = pack_uniforms(&out, &row);
	assert_eq!(buf.len(), out.uniform_block_bytes);
	let count = out
		.uniforms
		.iter()
		.find(|u| u.name == "point_count")
		.unwrap();
	assert_eq!(
		i32::from_le_bytes(buf[count.offset..count.offset + 4].try_into().unwrap()),
		3
	);
	let points = out.uniforms.iter().find(|u| u.name == "points_in").unwrap();
	let at = |i: usize, c: usize| points.offset + i * 16 + c * 4;
	assert_eq!(
		f32::from_le_bytes(buf[at(0, 0)..at(0, 1)].try_into().unwrap()),
		1.0
	);
	assert_eq!(
		f32::from_le_bytes(buf[at(0, 1)..at(0, 2)].try_into().unwrap()),
		2.0
	);
	assert_eq!(
		f32::from_le_bytes(buf[at(1, 0)..at(1, 1)].try_into().unwrap()),
		3.0
	);
	// Short arrays pad the remaining slots to zero.
	assert_eq!(
		f32::from_le_bytes(buf[at(63, 0)..at(63, 1)].try_into().unwrap()),
		0.0
	);
}

/// Uniform packing follows the declared types and std140 offsets.
#[test]
fn pack_uniforms_maps_node_values() {
	use quercus_node::value::{NodeValue, NodeValueRow};
	let glsl = r#"
uniform sampler2D tex_in;
uniform float gain_in;
uniform bool flag_in;
uniform vec4 color_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() { frag_color = texture(tex_in, ove_texcoord); }
"#;
	let out = translate(glsl).unwrap();
	let mut row = NodeValueRow::new();
	row.insert("gain_in".into(), NodeValue::Float(0.5));
	row.insert("flag_in".into(), NodeValue::Boolean(true));
	row.insert("color_in".into(), NodeValue::Color([0.1, 0.2, 0.3, 0.4]));
	let buf = pack_uniforms(&out, &row);
	assert_eq!(buf.len(), out.uniform_block_bytes);
	let gain = out.uniforms.iter().find(|u| u.name == "gain_in").unwrap();
	assert_eq!(
		f32::from_le_bytes(buf[gain.offset..gain.offset + 4].try_into().unwrap()),
		0.5
	);
	let flag = out.uniforms.iter().find(|u| u.name == "flag_in").unwrap();
	assert_eq!(
		i32::from_le_bytes(buf[flag.offset..flag.offset + 4].try_into().unwrap()),
		1
	);
	let color = out.uniforms.iter().find(|u| u.name == "color_in").unwrap();
	for (i, want) in [0.1f32, 0.2, 0.3, 0.4].iter().enumerate() {
		let at = color.offset + i * 4;
		assert_eq!(
			f32::from_le_bytes(buf[at..at + 4].try_into().unwrap()),
			*want
		);
	}
}

// ---- GPU runner tests (skipped without an adapter) -------------------

fn gpu() -> Option<std::sync::Arc<quercus_core::backend::GpuContext>> {
	quercus_core::backend::GpuContext::create(quercus_core::backend::BackendKind::Auto)
}

fn f32_frame(w: i32, h: i32, fill: impl Fn(usize) -> [f32; 4]) -> quercus_core::texture::Frame {
	use quercus_core::texture::Frame;
	let mut frame = Frame::new();
	let mut pod = quercus_core::frame::VideoParamsPod::default();
	pod.width = w;
	pod.height = h;
	pod.format = quercus_core::PixelFormat::F32 as i32;
	frame.set_video_params(pod);
	frame.allocate();
	for px in 0..(w * h) as usize {
		let rgba = fill(px);
		for (c, v) in rgba.iter().enumerate() {
			frame.data[(px * 4 + c) * 4..(px * 4 + c) * 4 + 4].copy_from_slice(&v.to_le_bytes());
		}
	}
	frame
}

fn pixel(out: &quercus_core::texture::Frame, x: usize) -> [f32; 4] {
	let mut rgba = [0.0f32; 4];
	for (c, v) in rgba.iter_mut().enumerate() {
		*v = f32::from_le_bytes(
			out.data[(x * 4 + c) * 4..(x * 4 + c) * 4 + 4]
				.try_into()
				.unwrap(),
		);
	}
	rgba
}

/// End-to-end effect pass: a translated node shader (gain multiply)
/// runs through `compile_shader_pass`/`run_shader_pass` and the
/// readback matches the expected pixels exactly. (Moved from
/// quercus-core's backend tests — the GLSL→WGSL translation and uniform
/// packing live in this module.)
#[test]
fn gpu_effect_pass_runs_translated_shader() {
	use quercus_core::frame::VideoParamsPod;
	use quercus_core::texture::Frame;

	let Some(ctx) = gpu() else {
		eprintln!("no adapter; skipping effect pass");
		return;
	};
	let glsl = r#"
uniform sampler2D tex_in;
uniform float gain_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
    frag_color = texture(tex_in, ove_texcoord) * gain_in;
}
"#;
	let translated = translate(glsl).unwrap();
	let program = ctx
		.compile_shader_pass(
			"test-gain",
			&translated.wgsl,
			translated.textures.len() as u32,
			!translated.uniforms.is_empty(),
			false,
		)
		.unwrap();

	let mut row = quercus_node::value::NodeValueRow::new();
	row.insert("gain_in".into(), quercus_node::value::NodeValue::Float(0.5));
	let uniforms = pack_uniforms(&translated, &row);

	let w = 4;
	let h = 2;
	let src = ctx.create_texture(w, h).unwrap();
	let dst = ctx.create_texture(w, h).unwrap();
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = w;
	pod.height = h;
	frame.set_video_params(pod);
	frame.allocate();
	// Distinct values per pixel (F32 RGBA): 0.2/0.4/0.6/1.0 shifted
	// per pixel, so a UV mixup would be visible.
	for px in 0..(w * h) as usize {
		for c in 0..4 {
			let v = 0.2 + 0.1 * (px + c) as f32;
			frame.data[(px * 4 + c) * 4..(px * 4 + c) * 4 + 4].copy_from_slice(&v.to_le_bytes());
		}
	}
	ctx.upload(src, &frame).unwrap();
	ctx.run_shader_pass(&program, &uniforms, &[src], dst)
		.unwrap();
	let out = ctx.download(dst).unwrap();
	for px in 0..(w * h) as usize {
		for c in 0..4 {
			let at = (px * 4 + c) * 4;
			let got = f32::from_le_bytes(out.data[at..at + 4].try_into().unwrap());
			let want = (0.2 + 0.1 * (px + c) as f32) * 0.5;
			assert!(
				(got - want).abs() < 1e-6,
				"px {px} ch {c}: got {got}, want {want}"
			);
		}
	}
	ctx.destroy_texture(src);
	ctx.destroy_texture(dst);
}

/// The real opacity shader through the full runner: pixels are
/// multiplied by the factor (and the UV convention is identity —
/// a flip would move the non-uniform pixels around).
#[test]
fn gpu_opacity_effect_scales_pixels() {
	let Some(ctx) = gpu() else {
		eprintln!("no adapter; skipping");
		return;
	};
	let (_core, behavior) = quercus_node::factory::Factory::global()
		.create_any("org.olivevideoeditor.Olive.opacity")
		.unwrap();
	let glsl = behavior.shader_code("").unwrap();
	let effect = compile_effect(&ctx, "test/opacity", &glsl, false).unwrap();

	let frame = f32_frame(16, 4, |px| [0.2 + 0.01 * px as f32, 0.4, 0.6, 1.0]);
	let src = ctx.create_texture(16, 4).unwrap();
	ctx.upload(src, &frame).unwrap();
	let dst = ctx.create_texture(16, 4).unwrap();

	let mut row = quercus_node::value::NodeValueRow::new();
	row.insert(
		"opacity_in".into(),
		quercus_node::value::NodeValue::Float(0.5),
	);
	run_effect(
		&ctx,
		&effect,
		&row,
		&[("tex_in".to_string(), src)],
		dst,
		(16, 4),
		1,
		None,
	)
	.unwrap();

	let out = ctx.download(dst).unwrap();
	for px in 0..16usize {
		let want = (0.2 + 0.01 * px as f32) * 0.5;
		let got = pixel(&out, px)[0];
		assert!((got - want).abs() < 1e-4, "px {px}: got {got}, want {want}");
	}
	ctx.destroy_texture(src);
	ctx.destroy_texture(dst);
}

/// The real blur shader: radius 0 is an exact passthrough (the
/// shader's MODE_NONE branch), and a 2px horizontal box blur on a
/// step edge lands exactly half-and-half at the boundary pixels.
#[test]
fn gpu_blur_effect_passthrough_and_step() {
	let Some(ctx) = gpu() else {
		eprintln!("no adapter; skipping");
		return;
	};
	let (_core, behavior) = quercus_node::factory::Factory::global()
		.create_any("org.olivevideoeditor.Olive.blur")
		.unwrap();
	let glsl = behavior.shader_code("").unwrap();
	let effect = compile_effect(&ctx, "test/blur", &glsl, false).unwrap();

	let src = ctx.create_texture(16, 1).unwrap();
	let dst = ctx.create_texture(16, 1).unwrap();
	let step = f32_frame(16, 1, |px| {
		if px < 8 {
			[0.0, 0.0, 0.0, 1.0]
		} else {
			[1.0, 1.0, 1.0, 1.0]
		}
	});
	ctx.upload(src, &step).unwrap();

	// radius 0: passthrough.
	let mut row = quercus_node::value::NodeValueRow::new();
	row.insert("method_in".into(), quercus_node::value::NodeValue::Combo(0));
	row.insert(
		"radius_in".into(),
		quercus_node::value::NodeValue::Float(0.0),
	);
	row.insert(
		"horiz_in".into(),
		quercus_node::value::NodeValue::Boolean(true),
	);
	row.insert(
		"vert_in".into(),
		quercus_node::value::NodeValue::Boolean(false),
	);
	run_effect(
		&ctx,
		&effect,
		&row,
		&[("tex_in".to_string(), src)],
		dst,
		(16, 1),
		1,
		None,
	)
	.unwrap();
	let out = ctx.download(dst).unwrap();
	assert_eq!(out.data, step.data, "radius 0 is a passthrough");

	// radius 2 horizontal box: out(x) = 0.5 * (in[x-1] + in[x+1]).
	row.insert(
		"radius_in".into(),
		quercus_node::value::NodeValue::Float(2.0),
	);
	run_effect(
		&ctx,
		&effect,
		&row,
		&[("tex_in".to_string(), src)],
		dst,
		(16, 1),
		1,
		None,
	)
	.unwrap();
	let out = ctx.download(dst).unwrap();
	for x in 0..16usize {
		let got = pixel(&out, x)[0];
		let want = match x {
			0 => 0.0, // first tap out of bounds (repeat_edge off)
			7 | 8 => 0.5,
			15 => 0.5, // second tap out of bounds
			_ if x < 7 => 0.0,
			_ => 1.0,
		};
		assert!((got - want).abs() < 1e-4, "px {x}: got {got}, want {want}");
	}
	ctx.destroy_texture(src);
	ctx.destroy_texture(dst);
}
/// The polygon generator's shader rasterizes the default pentagon on
/// the GPU: the center texel inside the closed point loop is opaque
/// white, the frame corner is transparent, and the 1-point fallback
/// renders nothing.
#[test]
fn gpu_polygon_rasterizes_pentagon() {
	let Some(ctx) = gpu() else {
		eprintln!("no adapter; skipping");
		return;
	};
	let (_core, behavior) = quercus_node::factory::Factory::global()
		.create_any("org.olivevideoeditor.Olive.polygon")
		.unwrap();
	let glsl = behavior.shader_code("rgb").unwrap();
	let effect = compile_effect(&ctx, "test/polygon", &glsl, false).unwrap();

	let dst = ctx.create_texture(512, 512).unwrap();
	let mut row = quercus_node::value::NodeValueRow::new();
	row.insert(
		"points_in".into(),
		quercus_node::value::NodeValue::Vec4Array(vec![
			[0.0, -135.0, 0.0, 0.0],
			[135.0, -45.0, 0.0, 0.0],
			[90.0, 120.0, 0.0, 0.0],
			[-90.0, 120.0, 0.0, 0.0],
			[-135.0, -45.0, 0.0, 0.0],
		]),
	);
	row.insert("point_count".into(), quercus_node::value::NodeValue::Int(5));
	row.insert(
		"color_in".into(),
		quercus_node::value::NodeValue::Color([1.0, 1.0, 1.0, 1.0]),
	);
	run_effect(&ctx, &effect, &row, &[], dst, (512, 512), 1, None).unwrap();
	let out = ctx.download(dst).unwrap();

	let center = pixel(&out, 256 * 512 + 256);
	assert_eq!(
		center,
		[1.0, 1.0, 1.0, 1.0],
		"center is inside the pentagon"
	);
	let corner = pixel(&out, 0);
	assert_eq!(corner, [0.0, 0.0, 0.0, 0.0], "corner is outside");

	// Degenerate: a single point draws nothing.
	row.insert("point_count".into(), quercus_node::value::NodeValue::Int(1));
	run_effect(&ctx, &effect, &row, &[], dst, (512, 512), 1, None).unwrap();
	let out = ctx.download(dst).unwrap();
	assert_eq!(
		pixel(&out, 256 * 512 + 256),
		[0.0, 0.0, 0.0, 0.0],
		"single point draws nothing"
	);

	ctx.destroy_texture(dst);
}

/// The mask effect's shader multiplies the base texture by the
/// pentagon matte on the GPU: the center texel keeps the base value,
/// the corner is cleared, and with `invert_in` the result flips.
#[test]
fn gpu_mask_multiplies_base_by_pentagon() {
	let Some(ctx) = gpu() else {
		eprintln!("no adapter; skipping");
		return;
	};
	let (_core, behavior) = quercus_node::factory::Factory::global()
		.create_any("org.olivevideoeditor.Olive.mask")
		.unwrap();
	let glsl = behavior.shader_code("mask").unwrap();
	let effect = compile_effect(&ctx, "test/mask", &glsl, false).unwrap();

	let src = ctx.create_texture(512, 512).unwrap();
	let dst = ctx.create_texture(512, 512).unwrap();
	let gray = crate::shaderfx::tests::f32_frame(512, 512, |_| [0.5, 0.5, 0.5, 1.0]);
	ctx.upload(src, &gray).unwrap();

	let mut row = quercus_node::value::NodeValueRow::new();
	row.insert(
		"points_in".into(),
		quercus_node::value::NodeValue::Vec4Array(vec![
			[0.0, -135.0, 0.0, 0.0],
			[135.0, -45.0, 0.0, 0.0],
			[90.0, 120.0, 0.0, 0.0],
			[-90.0, 120.0, 0.0, 0.0],
			[-135.0, -45.0, 0.0, 0.0],
		]),
	);
	row.insert("point_count".into(), quercus_node::value::NodeValue::Int(5));
	row.insert(
		"feather_in".into(),
		quercus_node::value::NodeValue::Float(0.0),
	);
	row.insert(
		"invert_in".into(),
		quercus_node::value::NodeValue::Boolean(false),
	);
	run_effect(
		&ctx,
		&effect,
		&row,
		&[("base_in".to_string(), src)],
		dst,
		(512, 512),
		1,
		None,
	)
	.unwrap();
	let out = ctx.download(dst).unwrap();
	let center = pixel(&out, 256 * 512 + 256);
	assert_eq!(center, [0.5, 0.5, 0.5, 1.0], "center keeps the base");
	assert_eq!(pixel(&out, 0), [0.0, 0.0, 0.0, 0.0], "corner is masked out");

	// Inverted: the corner keeps the base, the center is cleared.
	row.insert(
		"invert_in".into(),
		quercus_node::value::NodeValue::Boolean(true),
	);
	run_effect(
		&ctx,
		&effect,
		&row,
		&[("base_in".to_string(), src)],
		dst,
		(512, 512),
		1,
		None,
	)
	.unwrap();
	let out = ctx.download(dst).unwrap();
	assert_eq!(
		pixel(&out, 0),
		[0.5, 0.5, 0.5, 1.0],
		"inverted corner keeps base"
	);
	assert_eq!(
		pixel(&out, 256 * 512 + 256),
		[0.0, 0.0, 0.0, 0.0],
		"inverted center cleared"
	);

	// Feather: a square polygon with radius 4 softens the edge — the
	// pixel right outside the crisp edge becomes partially visible.
	row.insert(
		"invert_in".into(),
		quercus_node::value::NodeValue::Boolean(false),
	);
	row.insert(
		"feather_in".into(),
		quercus_node::value::NodeValue::Float(4.0),
	);
	row.insert(
		"points_in".into(),
		quercus_node::value::NodeValue::Vec4Array(vec![
			[-150.0, -150.0, 0.0, 0.0],
			[150.0, -150.0, 0.0, 0.0],
			[150.0, 150.0, 0.0, 0.0],
			[-150.0, 150.0, 0.0, 0.0],
		]),
	);
	row.insert("point_count".into(), quercus_node::value::NodeValue::Int(4));
	run_effect(
		&ctx,
		&effect,
		&row,
		&[("base_in".to_string(), src)],
		dst,
		(512, 512),
		1,
		None,
	)
	.unwrap();
	let out = ctx.download(dst).unwrap();
	let soft = pixel(&out, 408 * 512 + 256)[0];
	assert!(
		soft > 0.02 && soft < 0.6,
		"just outside the feathered edge is partially visible: {soft}"
	);

	ctx.destroy_texture(src);
	ctx.destroy_texture(dst);
}

/// The linear grading node's OCIO-spliced shader doubles gray under
/// +1 stop of master exposure (2^1 * 0.2 = 0.4) and gates the
/// transform behind the localBypass uniform off.
#[test]
fn gpu_grading_linear_applies_exposure() {
	let Some(ctx) = gpu() else {
		eprintln!("no adapter; skipping");
		return;
	};
	let Some(stub) =
		crate::eval::grading_stub_for("org.olivevideoeditor.Olive.ociogradingtransformlinear")
	else {
		eprintln!("no OCIO config; skipping");
		return;
	};
	let (_core, behavior) = quercus_node::factory::Factory::global()
		.create_any("org.olivevideoeditor.Olive.ociogradingtransformlinear")
		.unwrap();
	let glsl = behavior.shader_code(&stub).unwrap();
	let effect = compile_effect(&ctx, "test/grading-lin", &glsl, false).unwrap();

	let src = ctx.create_texture(8, 4).unwrap();
	let dst = ctx.create_texture(8, 4).unwrap();
	let gray = crate::shaderfx::tests::f32_frame(8, 4, |_| [0.2, 0.2, 0.2, 1.0]);
	ctx.upload(src, &gray).unwrap();

	let mut row = quercus_node::value::NodeValueRow::new();
	row.insert(
		"ocio_grading_primary_exposure".into(),
		quercus_node::value::NodeValue::Vec3([2.0, 2.0, 2.0]),
	);
	row.insert(
		"ocio_grading_primary_contrast".into(),
		quercus_node::value::NodeValue::Vec3([1.0, 1.0, 1.0]),
	);
	row.insert(
		"ocio_grading_primary_offset".into(),
		quercus_node::value::NodeValue::Vec3([0.0, 0.0, 0.0]),
	);
	row.insert(
		"ocio_grading_primary_saturation".into(),
		quercus_node::value::NodeValue::Float(1.0),
	);
	row.insert(
		"ocio_grading_primary_pivot".into(),
		quercus_node::value::NodeValue::Float(0.18),
	);
	row.insert(
		"ocio_grading_primary_clampBlack".into(),
		quercus_node::value::NodeValue::Float(-1.0),
	);
	row.insert(
		"ocio_grading_primary_clampWhite".into(),
		quercus_node::value::NodeValue::Float(2.0),
	);
	row.insert(
		"ocio_grading_primary_localBypass".into(),
		quercus_node::value::NodeValue::Boolean(false),
	);
	run_effect(
		&ctx,
		&effect,
		&row,
		&[("tex_in".to_string(), src)],
		dst,
		(8, 4),
		1,
		None,
	)
	.unwrap();
	let out = ctx.download(dst).unwrap();
	for px in 0..8usize {
		let got = pixel(&out, px);
		let want = [0.4f32, 0.4, 0.4, 1.0];
		assert!(
			(got[0] - want[0]).abs() < 1e-3,
			"px {px}: exposure must double gray, got {got:?}"
		);
	}

	ctx.destroy_texture(src);
	ctx.destroy_texture(dst);
}

/// The log grading node's OCIO-spliced shader: brightness (lift) of
/// +0.1 shifts 0.2 gray to 0.3 with the identity gain/gamma/pivot
/// defaults.
#[test]
fn gpu_grading_log_applies_lift() {
	let Some(ctx) = gpu() else {
		eprintln!("no adapter; skipping");
		return;
	};
	let Some(stub) = crate::eval::grading_stub_for(
		"org.olivevideoeditor.Olive.OCIO_NAMESPACEgradingtransformlog",
	) else {
		eprintln!("no OCIO config; skipping");
		return;
	};
	let (_core, behavior) = quercus_node::factory::Factory::global()
		.create_any("org.olivevideoeditor.Olive.OCIO_NAMESPACEgradingtransformlog")
		.unwrap();
	let glsl = behavior.shader_code(&stub).unwrap();
	let effect = compile_effect(&ctx, "test/grading-log", &glsl, false).unwrap();

	let src = ctx.create_texture(8, 4).unwrap();
	let dst = ctx.create_texture(8, 4).unwrap();
	let gray = crate::shaderfx::tests::f32_frame(8, 4, |_| [0.2, 0.2, 0.2, 1.0]);
	ctx.upload(src, &gray).unwrap();

	let mut row = quercus_node::value::NodeValueRow::new();
	row.insert(
		"ocio_grading_primary_brightness".into(),
		quercus_node::value::NodeValue::Vec3([0.1, 0.1, 0.1]),
	);
	row.insert(
		"ocio_grading_primary_contrast".into(),
		quercus_node::value::NodeValue::Vec3([1.0, 1.0, 1.0]),
	);
	row.insert(
		"ocio_grading_primary_gamma".into(),
		quercus_node::value::NodeValue::Vec3([1.0, 1.0, 1.0]),
	);
	row.insert(
		"ocio_grading_primary_saturation".into(),
		quercus_node::value::NodeValue::Float(1.0),
	);
	row.insert(
		"ocio_grading_primary_pivot".into(),
		quercus_node::value::NodeValue::Float(-0.2),
	);
	row.insert(
		"ocio_grading_primary_pivotBlack".into(),
		quercus_node::value::NodeValue::Float(0.0),
	);
	row.insert(
		"ocio_grading_primary_pivotWhite".into(),
		quercus_node::value::NodeValue::Float(1.0),
	);
	row.insert(
		"ocio_grading_primary_clampBlack".into(),
		quercus_node::value::NodeValue::Float(-1.0),
	);
	row.insert(
		"ocio_grading_primary_clampWhite".into(),
		quercus_node::value::NodeValue::Float(2.0),
	);
	row.insert(
		"ocio_grading_primary_localBypass".into(),
		quercus_node::value::NodeValue::Boolean(false),
	);
	run_effect(
		&ctx,
		&effect,
		&row,
		&[("tex_in".to_string(), src)],
		dst,
		(8, 4),
		1,
		None,
	)
	.unwrap();
	let out = ctx.download(dst).unwrap();
	for px in 0..8usize {
		let got = pixel(&out, px);
		assert!(
			(got[0] - 0.3).abs() < 1e-3,
			"px {px}: lift must shift gray, got {got:?}"
		);
	}

	ctx.destroy_texture(src);
	ctx.destroy_texture(dst);
}

/// Every registered node type that ships a shader must translate (the
/// all-shaders sweep). OCIO-stubbed shaders (`%1` markers needing the
/// OCIO-generated function text) retry with the real OCIO stub first;
/// only nodes whose stub is unavailable *and* unknown fail outright.
#[test]
fn all_registered_shaders_translate() {
	let mut ok = Vec::new();
	let mut ocio_stubbed = Vec::new();
	let mut failed = Vec::new();
	for meta in quercus_node::factory::Factory::global().entries() {
		let (_core, behavior) = (meta.create)();
		let Some(glsl) = behavior.shader_code("") else {
			continue;
		};
		match translate(&glsl) {
			Ok(_) => ok.push(meta.type_id),
			Err(e) => {
				let msg = format!("{e:?}");
				// The unresolved OCIO stub is the one sanctioned
				// failure mode (chromakey & co. call into
				// OCIO-generated functions). Retry with the real OCIO
				// stub: the wiring must make these translate. When no
				// OCIO config is available (stub build), fall back to
				// a pass-through function so the sweep still covers
				// the node's own shader body.
				let retried = crate::eval::ocio_stub_for(meta.type_id)
					.or_else(|| {
						crate::eval::OCIO_SHADER_STUBS
							.iter()
							.find(|(id, ..)| *id == meta.type_id)
							.map(|(_, fn_name, ..)| {
								format!("vec4 {fn_name}(vec4 c) {{ return c; }}")
							})
					})
					.or_else(|| crate::eval::grading_stub_for(meta.type_id))
					.or_else(|| {
						crate::eval::OCIO_GRADING_STUBS
							.iter()
							.find(|(id, _)| *id == meta.type_id)
							.map(|_| "vec4 ove_grading_primary(vec4 c) { return c; }".to_string())
					});
				match retried {
					Some(stub) => match translate(&behavior.shader_code(&stub).unwrap()) {
						Ok(_) => ok.push(meta.type_id),
						Err(e) => failed.push((meta.type_id, format!("with OCIO stub: {e:?}"))),
					},
					None if msg.contains("SceneLinear")
						|| msg.contains("UnknownFunction")
						|| msg.contains("ove_grading_primary") =>
					{
						// Not in the stub table but still OCIO-shaped:
						// report separately, not as a regression.
						ocio_stubbed.push(meta.type_id);
					}
					None => failed.push((meta.type_id, msg)),
				}
			}
		}
	}
	eprintln!(
		"shader sweep: {} ok, {} ocio-stubbed",
		ok.len(),
		ocio_stubbed.len()
	);
	for id in &ocio_stubbed {
		eprintln!("  ocio-stubbed: {id}");
	}
	for (id, msg) in &failed {
		eprintln!("  FAILED: {id}: {}", &msg[..msg.len().min(200)]);
	}
	assert!(
		failed.is_empty(),
		"{} shaders failed to translate",
		failed.len()
	);
	assert!(!ok.is_empty(), "no shaders translated at all");
}
