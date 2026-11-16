use super::*;
use crate::value::{NodeValueTable, SampleBuffer};
use quercus_core::{Rational, SampleFormat};

fn table(values: Vec<NodeValue>) -> NodeValueTable {
	let mut t = NodeValueTable::default();
	for v in values {
		let ty = v.value_type();
		t.push(ty, v, None);
	}
	t
}

#[test]
fn operation_names() {
	assert_eq!(MathNodeBase::operation_name(Operation::Add), "Add");
	assert_eq!(
		MathNodeBase::operation_name(Operation::Subtract),
		"Subtract"
	);
	assert_eq!(
		MathNodeBase::operation_name(Operation::Multiply),
		"Multiply"
	);
	assert_eq!(MathNodeBase::operation_name(Operation::Divide), "Divide");
	assert_eq!(MathNodeBase::operation_name(Operation::Power), "Power");
}

#[test]
fn number_is_no_op_semantics() {
	// Add/subtract: exactly 0.0.
	assert!(MathNodeBase::number_is_no_op(Operation::Add, 0.0));
	assert!(MathNodeBase::number_is_no_op(Operation::Subtract, 0.0));
	assert!(!MathNodeBase::number_is_no_op(Operation::Add, 0.5));
	// Multiply/divide/power: fuzzy 1.0 (float factor 1e5).
	assert!(MathNodeBase::number_is_no_op(Operation::Multiply, 1.0));
	assert!(MathNodeBase::number_is_no_op(Operation::Divide, 1.0));
	assert!(MathNodeBase::number_is_no_op(Operation::Power, 1.0));
	assert!(MathNodeBase::number_is_no_op(
		Operation::Multiply,
		1.0 + 0.0000001
	));
	assert!(!MathNodeBase::number_is_no_op(Operation::Multiply, 2.0));
	assert!(!MathNodeBase::number_is_no_op(Operation::Divide, 0.0));
}

#[test]
fn pairing_calculator_picks_number_number() {
	let a = table(vec![NodeValue::Float(2.0)]);
	let b = table(vec![NodeValue::Float(3.0)]);
	let p = PairingCalculator::new(&a, &b);
	assert!(p.found_most_likely_pairing());
	assert_eq!(p.most_likely_pairing, Pairing::NumberNumber);
	assert_eq!(p.most_likely_value_a, NodeValue::Float(2.0));
	assert_eq!(p.most_likely_value_b, NodeValue::Float(3.0));
}

#[test]
fn pairing_calculator_empty_tables_find_nothing() {
	let a = NodeValueTable::default();
	let b = NodeValueTable::default();
	let p = PairingCalculator::new(&a, &b);
	assert!(!p.found_most_likely_pairing());
	assert_eq!(p.most_likely_pairing, Pairing::None);
	assert_eq!(p.most_likely_value_a, NodeValue::None);
}

#[test]
fn pairing_calculator_vector_pairing() {
	let a = table(vec![NodeValue::Vec2([1.0, 2.0])]);
	let b = table(vec![NodeValue::Vec2([3.0, 4.0])]);
	let p = PairingCalculator::new(&a, &b);
	assert_eq!(p.most_likely_pairing, Pairing::VecVec);
}

#[test]
fn pairing_calculator_length_weight_bonus() {
	// Two rows vs one: the two-row table wins the tie for a pairing
	// both can serve.
	let a = table(vec![NodeValue::Float(1.0), NodeValue::Float(2.0)]);
	let b = table(vec![NodeValue::Float(3.0)]);
	let p = PairingCalculator::new(&a, &b);
	assert_eq!(p.most_likely_pairing, Pairing::NumberNumber);
}

#[test]
fn value_internal_number_number_float() {
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Add,
		Pairing::NumberNumber,
		"a",
		&NodeValue::Float(2.0),
		"b",
		&NodeValue::Float(3.0),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert_eq!(out.get(ValueType::Float), Some(&NodeValue::Float(5.0)));
}

#[test]
fn value_internal_number_number_rational_preserved() {
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Add,
		Pairing::NumberNumber,
		"a",
		&NodeValue::Rational(Rational::new(1, 2)),
		"b",
		&NodeValue::Rational(Rational::new(1, 3)),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert_eq!(
		out.get(ValueType::Rational),
		Some(&NodeValue::Rational(Rational::new(5, 6)))
	);
	// Power falls through to float.
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Power,
		Pairing::NumberNumber,
		"a",
		&NodeValue::Rational(Rational::new(2, 1)),
		"b",
		&NodeValue::Rational(Rational::new(3, 1)),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert_eq!(out.get(ValueType::Float), Some(&NodeValue::Float(8.0)));
}

#[test]
fn value_internal_vec_vec_divide_padding_guard() {
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Divide,
		Pairing::VecVec,
		"a",
		&NodeValue::Vec2([1.0, 4.0]),
		"b",
		&NodeValue::Vec2([2.0, 2.0]),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert_eq!(out.get(ValueType::Vec2), Some(&NodeValue::Vec2([0.5, 2.0])));
}

#[test]
fn value_internal_vec_number_multiply() {
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Multiply,
		Pairing::VecNumber,
		"a",
		&NodeValue::Vec3([1.0, 2.0, 3.0]),
		"b",
		&NodeValue::Float(2.0),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert_eq!(
		out.get(ValueType::Vec3),
		Some(&NodeValue::Vec3([2.0, 4.0, 6.0]))
	);
}

#[test]
fn value_internal_matrix_vec() {
	// Translate is stored at row 0, col 3 (C++ `Matrix4x4::translate`).
	// `v * m` computes result[i] = sum_j v[j] * m[j][i], so the
	// translation lands in the w component, not x.
	let mut m = [0.0f64; 16];
	m[0] = 1.0;
	m[5] = 1.0;
	m[10] = 1.0;
	m[15] = 1.0;
	m[3] = 100.0; // m[0][3] translation x
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Multiply,
		Pairing::MatrixVec,
		"a",
		&NodeValue::Matrix(m),
		"b",
		&NodeValue::Vec2([10.0, 20.0]),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	// x' = 10*m[0][0] = 10; y' = 20*m[1][1] = 20.
	assert_eq!(
		out.get(ValueType::Vec2),
		Some(&NodeValue::Vec2([10.0, 20.0]))
	);
}

#[test]
fn value_internal_color_ops() {
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Add,
		Pairing::ColorColor,
		"a",
		&NodeValue::Color([1.0, 0.0, 0.0, 1.0]),
		"b",
		&NodeValue::Color([0.5, 0.5, 0.0, 0.0]),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert_eq!(
		out.get(ValueType::Color),
		Some(&NodeValue::Color([1.5, 0.5, 0.0, 1.0]))
	);

	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Multiply,
		Pairing::NumberColor,
		"a",
		&NodeValue::Float(0.5),
		"b",
		&NodeValue::Color([1.0, 1.0, 1.0, 1.0]),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert_eq!(
		out.get(ValueType::Color),
		Some(&NodeValue::Color([0.5, 0.5, 0.5, 0.5]))
	);
}

fn f32_planar(channels: usize, count: usize, values: &[f32]) -> SampleBuffer {
	let mut buf = SampleBuffer {
		format: SampleFormat::F32Planar,
		channels,
		sample_count: count,
		data: vec![0u8; channels * count * 4],
	};
	for c in 0..channels {
		for i in 0..count {
			let v = values.get(c * count + i).copied().unwrap_or(0.0);
			buf.set_sample_value(c, i, v as f64);
		}
	}
	buf
}

#[test]
fn value_internal_sample_sample_mix() {
	let a = NodeValue::Samples(f32_planar(1, 4, &[1.0, 2.0, 3.0, 4.0]));
	let b = NodeValue::Samples(f32_planar(1, 2, &[0.5, 0.5]));
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Add,
		Pairing::SampleSample,
		"a",
		&a,
		"b",
		&b,
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	let mixed = match out.get(ValueType::Samples).unwrap() {
		NodeValue::Samples(s) => s,
		_ => panic!("samples expected"),
	};
	assert_eq!(mixed.sample_count, 4);
	assert_eq!(mixed.sample_value(0, 0), 1.5);
	assert_eq!(mixed.sample_value(0, 1), 2.5);
	assert_eq!(mixed.sample_value(0, 2), 3.0, "tail copied from a");
	assert_eq!(mixed.sample_value(0, 3), 4.0);
}

#[test]
fn value_internal_sample_number_static() {
	let buf = f32_planar(2, 2, &[1.0, 2.0, 3.0, 4.0]);
	let samples = NodeValue::Samples(buf.clone());
	let mut out = NodeValueTable::default();
	let core = NodeCore::new();
	let inputs = NodeValueRow::default();
	MathNodeBase::value_internal(
		Operation::Multiply,
		Pairing::SampleNumber,
		"samples_in",
		&samples,
		"number_in",
		&NodeValue::Float(2.0),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&core,
		&inputs,
		&mut out,
	);
	let out_buf = match out.get(ValueType::Samples).unwrap() {
		NodeValue::Samples(s) => s,
		_ => panic!("samples expected"),
	};
	assert_eq!(out_buf.sample_value(0, 0), 2.0);
	assert_eq!(out_buf.sample_value(0, 1), 4.0);
	assert_eq!(out_buf.sample_value(1, 0), 6.0);
	assert_eq!(out_buf.sample_value(1, 1), 8.0);
	// The static fast path must not mutate the input.
	assert_eq!(buf.sample_value(0, 0), 1.0);
}

#[test]
fn value_internal_sample_number_noop_passes_through() {
	let buf = f32_planar(1, 2, &[1.0, 2.0]);
	let samples = NodeValue::Samples(buf.clone());
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Multiply,
		Pairing::SampleNumber,
		"samples_in",
		&samples,
		"number_in",
		&NodeValue::Float(1.0),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert_eq!(out.get(ValueType::Samples), Some(&NodeValue::Samples(buf)));
}

#[test]
fn shader_code_number_number_add() {
	let (frag, vert) = MathNodeBase::shader_code_internal("0.0.2.2", "param_a_in", "param_b_in");
	assert!(vert.is_empty());
	assert!(frag.contains("uniform float param_a_in;"));
	assert!(frag.contains("uniform float param_b_in;"));
	assert!(frag.contains("vec4 c = param_a_in + param_b_in;"));
	assert!(frag.contains("c.a = clamp(c.a, 0.0, 1.0);"));
}

#[test]
fn shader_code_texture_matrix_multiply() {
	let (frag, vert) = MathNodeBase::shader_code_internal("2.10.10.6", "tex_in", "mat_in");
	assert!(frag.contains("texture(tex_in, ove_texcoord)"));
	assert!(vert.contains("uniform mat4 mat_in;"));
	assert!(vert.contains("gl_Position = mat_in * a_position;"));
}

#[test]
fn shader_code_power_texture_number_wraps_vec4() {
	// op=4 (power), pairing=8 (texture_number), type_a=10 (texture),
	// type_b=2 (float): pow(texture, vec4(number)).
	let (frag, _) = MathNodeBase::shader_code_internal("4.8.10.2", "tex_in", "num_in");
	assert!(frag.contains("pow(texture(tex_in, ove_texcoord), vec4(num_in))"));
}

#[test]
fn process_samples_multiply() {
	let input = f32_planar(2, 3, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
	let mut output = SampleBuffer {
		format: SampleFormat::F32Planar,
		channels: 2,
		sample_count: 3,
		data: vec![0u8; 2 * 3 * 4],
	};
	let values = NodeValueRow::from([
		("samples_in".to_string(), NodeValue::Samples(input.clone())),
		("volume_in".to_string(), NodeValue::Float(0.5)),
	]);
	MathNodeBase::process_samples_internal(
		&values,
		Operation::Multiply,
		"samples_in",
		"volume_in",
		&input,
		&mut output,
		1,
	);
	assert_eq!(output.sample_value(0, 1), 1.0, "ch0 index1 = 2 * 0.5");
	assert_eq!(output.sample_value(1, 1), 2.5, "ch1 index1 = 5 * 0.5");
}

#[test]
fn retrieve_number_rational_and_float() {
	assert_eq!(MathNodeBase::retrieve_number(&NodeValue::Float(3.5)), 3.5);
	assert_eq!(
		MathNodeBase::retrieve_number(&NodeValue::Rational(Rational::new(1, 2))),
		0.5
	);
	assert_eq!(MathNodeBase::retrieve_number(&NodeValue::Int(4)), 4.0);
}

#[test]
fn matrix_helpers() {
	assert!(matrix_is_identity(identity_matrix()));

	let mut m = identity_matrix();
	m[3] = 5.0;
	assert!(!matrix_is_identity(m));
	let out = vec_mul_matrix([1.0, 0.0, 0.0, 1.0], m);
	// v * m: m[3] (row 0, col 3) contributes to the w component.
	assert_eq!(out[0], 1.0);
	assert_eq!(out[3], 6.0);

	// matrix * matrix: identity * identity = identity.
	let prod = add_sub_mult_matrix(Operation::Multiply, identity_matrix(), identity_matrix());
	assert!(matrix_is_identity(prod));
}

#[test]
fn value_texture_number_noop_pushes_texture_as_is() {
	let tex = NodeValue::Texture(crate::handle::CHandle::null());
	// A null handle is treated as "no texture" -> noop passthrough.
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Multiply,
		Pairing::TextureNumber,
		"tex_in",
		&tex,
		"num_in",
		&NodeValue::Float(1.0),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert_eq!(
		out.get(ValueType::Texture),
		Some(&tex),
		"null texture passthrough"
	);
}

#[test]
fn value_texture_number_identity_number_pushes_through() {
	// Non-null texture handle + identity number: noop -> passthrough.
	// Use a boxed handle so it is non-null but drops safely.
	let tex = crate::value::NodeValue::Texture(crate::handle::make_owned::<u8>(7));
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Multiply,
		Pairing::TextureNumber,
		"tex_in",
		&tex,
		"num_in",
		&NodeValue::Float(1.0),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	let pushed = out.get(ValueType::Texture).unwrap();
	match pushed {
		NodeValue::Texture(h) => assert!(!h.is_null()),
		_ => panic!("texture"),
	}
}

#[test]
fn value_texture_number_pushes_job_payload() {
	let tex = crate::value::NodeValue::Texture(crate::handle::make_owned::<u8>(7));
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Multiply,
		Pairing::TextureNumber,
		"tex_in",
		&tex,
		"num_in",
		&NodeValue::Float(2.0),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	let handle = match out.get(ValueType::Texture).unwrap() {
		NodeValue::Texture(h) => *h,
		_ => panic!("texture expected"),
	};
	let payload = unsafe { crate::jobs::shader_job(&handle) }
		.expect("shader job payload boxed in the pushed texture");
	assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.math");
	// op=2 (multiply), pairing=8 (texture_number), a=10 (texture),
	// b=2 (float).
	assert_eq!(payload.shader_id, "2.8.10.2");
	assert_eq!(payload.iterations, 1);
	assert_eq!(payload.effect_input, "");
	assert_eq!(payload.params.get("tex_in"), Some(&tex));
	assert_eq!(payload.params.get("num_in"), Some(&NodeValue::Float(2.0)));
}

#[test]
fn value_texture_matrix_identity_noop() {
	let tex = crate::value::NodeValue::Texture(crate::handle::make_owned::<u8>(7));
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Multiply,
		Pairing::TextureMatrix,
		"tex_in",
		&tex,
		"mat_in",
		&NodeValue::Matrix(identity_matrix()),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	match out.get(ValueType::Texture).unwrap() {
		NodeValue::Texture(h) => assert!(!h.is_null(), "identity matrix passthrough"),
		_ => panic!("texture"),
	}
}

#[test]
fn value_sample_number_dynamic_passes_through() {
	// A keyframed number operand makes the input non-static.
	let mut core = NodeCore::new();
	core.keyframe_track_mut("num_in", -1)
		.set_key(crate::keyframe::Keyframe {
			time: quercus_core::Rational::new(0, 1),
			value: NodeValue::Float(2.0),
			interpolation: crate::keyframe::Interpolation::Linear,
			bezier_in: (0.0, 0.0),
			bezier_out: (0.0, 0.0),
		});
	let buf = crate::value::SampleBuffer {
		format: quercus_core::SampleFormat::F32Planar,
		channels: 1,
		sample_count: 1,
		data: vec![0u8; 4],
	};
	let samples = NodeValue::Samples(buf);
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Multiply,
		Pairing::SampleNumber,
		"samples_in",
		&samples,
		"num_in",
		&NodeValue::Float(2.0),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&core,
		&NodeValueRow::default(),
		&mut out,
	);
	match out.get(ValueType::Samples).unwrap() {
		NodeValue::Samples(s) => assert_eq!(s.sample_value(0, 0), 0.0),
		_ => panic!("samples"),
	}
}

#[test]
fn value_vec_vec_promotes_type() {
	// vec2 + vec3 -> vec3.
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Add,
		Pairing::VecVec,
		"a",
		&NodeValue::Vec2([1.0, 2.0]),
		"b",
		&NodeValue::Vec3([1.0, 1.0, 1.0]),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert_eq!(
		out.get(ValueType::Vec3),
		Some(&NodeValue::Vec3([2.0, 3.0, 1.0]))
	);
}

#[test]
fn value_vec_number_divide() {
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Divide,
		Pairing::VecNumber,
		"a",
		&NodeValue::Vec2([4.0, 8.0]),
		"b",
		&NodeValue::Float(2.0),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert_eq!(out.get(ValueType::Vec2), Some(&NodeValue::Vec2([2.0, 4.0])));
}

#[test]
fn value_matrix_matrix_ops() {
	// Add and subtract are element-wise; multiply is the product.
	let mut a = identity_matrix();
	a[0] = 2.0;
	let b = identity_matrix();
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Multiply,
		Pairing::MatrixMatrix,
		"a",
		&NodeValue::Matrix(a),
		"b",
		&NodeValue::Matrix(b),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	match out.get(ValueType::Matrix).unwrap() {
		NodeValue::Matrix(m) => assert_eq!(m[0], 2.0),
		_ => panic!("matrix"),
	}
}

#[test]
fn value_rational_arithmetic_all_ops() {
	use quercus_core::Rational;
	for (op, a, b, expect) in [
		(
			Operation::Add,
			Rational::new(1, 2),
			Rational::new(1, 3),
			Rational::new(5, 6),
		),
		(
			Operation::Subtract,
			Rational::new(1, 2),
			Rational::new(1, 3),
			Rational::new(1, 6),
		),
		(
			Operation::Multiply,
			Rational::new(2, 3),
			Rational::new(3, 4),
			Rational::new(1, 2),
		),
		(
			Operation::Divide,
			Rational::new(1, 2),
			Rational::new(1, 4),
			Rational::new(2, 1),
		),
	] {
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			op,
			Pairing::NumberNumber,
			"a",
			&NodeValue::Rational(a),
			"b",
			&NodeValue::Rational(b),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(
			out.get(ValueType::Rational),
			Some(&NodeValue::Rational(expect))
		);
	}
}

#[test]
fn pairing_calculator_sample_and_texture() {
	let a = table(vec![NodeValue::Samples(
		crate::value::SampleBuffer::default(),
	)]);
	let b = table(vec![NodeValue::Float(1.0)]);
	let p = PairingCalculator::new(&a, &b);
	assert_eq!(p.most_likely_pairing, Pairing::SampleNumber);

	let a = table(vec![NodeValue::Texture(crate::handle::CHandle::null())]);
	let b = table(vec![NodeValue::Texture(crate::handle::CHandle::null())]);
	let p = PairingCalculator::new(&a, &b);
	assert_eq!(p.most_likely_pairing, Pairing::TextureTexture);
}

#[test]
fn shader_code_more_variants() {
	// Subtract (1) number-number: a - b.
	let (frag, _) = MathNodeBase::shader_code_internal("1.0.2.2", "a", "b");
	assert!(frag.contains("vec4 c = a - b;"));
	// Divide (3) texture-number: texture(a) / number.
	let (frag, _) = MathNodeBase::shader_code_internal("3.8.10.2", "a", "b");
	assert!(frag.contains("vec4 c = texture(a, ove_texcoord) / b;"));
	// Multiply (2) texture-texture: texture * texture.
	let (frag, _) = MathNodeBase::shader_code_internal("2.4.10.10", "a", "b");
	assert!(frag.contains("texture(a, ove_texcoord) * texture(b, ove_texcoord)"));
	// Power (4) number-color: pow(number, color) (number on the left).
	let (frag, _) = MathNodeBase::shader_code_internal("4.7.2.5", "a", "b");
	assert!(frag.contains("pow(a, b)"));
	// Malformed id returns empty strings.
	let (frag, vert) = MathNodeBase::shader_code_internal("not-an-id", "a", "b");
	assert!(frag.is_empty());
	assert!(vert.is_empty());
}

#[test]
fn process_samples_falls_back_when_param_a_not_scalar() {
	let input = crate::value::SampleBuffer {
		format: quercus_core::SampleFormat::F32Planar,
		channels: 1,
		sample_count: 1,
		data: vec![0u8; 4],
	};
	let mut output = input.clone();
	let values = NodeValueRow::from([
		("samples_in".to_string(), NodeValue::Samples(input.clone())),
		("volume_in".to_string(), NodeValue::Float(2.0)),
	]);
	// param_a_in is "samples_in" (not a scalar) -> falls back to volume.
	MathNodeBase::process_samples_internal(
		&values,
		Operation::Multiply,
		"samples_in",
		"volume_in",
		&input,
		&mut output,
		0,
	);
	// input value is 0.0 (empty data); no crash is the main check.
	let values2 = NodeValueRow::from([
		("samples_in".to_string(), NodeValue::Samples(input.clone())),
		("volume_in".to_string(), NodeValue::Float(2.0)),
	]);
	let mut input2 = input.clone();
	input2.set_sample_value(0, 0, 3.0);
	let mut output2 = input2.clone();
	MathNodeBase::process_samples_internal(
		&values2,
		Operation::Multiply,
		"samples_in",
		"volume_in",
		&input2,
		&mut output2,
		0,
	);
	assert_eq!(
		output2.sample_value(0, 0),
		6.0,
		"3 * 2 from the fallback scalar"
	);
}

#[test]
fn retrieve_number_combo_and_bool() {
	assert_eq!(MathNodeBase::retrieve_number(&NodeValue::Combo(3)), 3.0);
	assert_eq!(
		MathNodeBase::retrieve_number(&NodeValue::Boolean(true)),
		1.0
	);
	assert_eq!(MathNodeBase::retrieve_number(&NodeValue::None), 0.0);
}

#[test]
fn is_scalar_checks() {
	assert!(is_scalar(Some(&NodeValue::Float(1.0))));
	assert!(is_scalar(Some(&NodeValue::Rational(
		quercus_core::Rational::new(1, 2)
	))));
	assert!(!is_scalar(Some(&NodeValue::Samples(
		crate::value::SampleBuffer::default()
	))));
	assert!(!is_scalar(Some(&NodeValue::None)));
	assert!(!is_scalar(None));
}

#[test]
fn color_color_unsupported_op_returns_a() {
	// multiply is not valid for color+color -> returns a unchanged.
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Multiply,
		Pairing::ColorColor,
		"a",
		&NodeValue::Color([1.0, 2.0, 3.0, 4.0]),
		"b",
		&NodeValue::Color([0.5, 0.5, 0.5, 0.5]),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert_eq!(
		out.get(ValueType::Color),
		Some(&NodeValue::Color([1.0, 2.0, 3.0, 4.0]))
	);
}

#[test]
fn value_internal_none_pairing_noop() {
	let mut out = NodeValueTable::default();
	MathNodeBase::value_internal(
		Operation::Add,
		Pairing::None,
		"a",
		&NodeValue::Float(1.0),
		"b",
		&NodeValue::Float(2.0),
		Rational::new(0, 1),
		"org.olivevideoeditor.Olive.math",
		&NodeCore::new(),
		&NodeValueRow::default(),
		&mut out,
	);
	assert!(out.is_empty());
}
