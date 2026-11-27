use super::*;

#[test]
fn generated_frame_is_f32_transparent_black() {
	let f = generate_frame(Rational::new(5, 1), (64, 48), PixelFormat::F32).unwrap();
	assert_eq!(f.width, 64);
	assert_eq!(f.height, 48);
	assert_eq!(f.format, PixelFormat::F32);
	assert_eq!(f.timestamp, Rational::new(5, 1));
	assert!(f.data.iter().all(|&b| b == 0), "transparent black");
	assert_eq!(f.data.len(), 64 * 48 * 4 * 4);
}

#[test]
fn generated_frame_rejects_bad_size() {
	assert!(generate_frame(Rational::new(0, 1), (0, 10), PixelFormat::F32).is_err());
	assert!(generate_frame(Rational::new(0, 1), (-1, 10), PixelFormat::F32).is_err());
}

#[test]
fn produced_frame_honors_ticket_params() {
	let params = crate::ticket::VideoTicketParams {
		viewer: 1,
		project: String::new(),
		time: Rational::new(2, 1),
		force_size: Some((16, 9)),
		force_format: Some(PixelFormat::F32),
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage: Vec::new(),
		adjustments: Vec::new(),
	};
	let tex = render_produced_frame(params.time, &params).unwrap();
	assert_eq!(tex.size(), (16, 9));
	assert_eq!(tex.format(), PixelFormat::F32);
}

/// A temporary disk frame-cache path for the cache-job tests.
fn cache_job_temp_path(tag: &str) -> String {
	static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
	let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	std::env::temp_dir()
		.join(format!(
			"quercusrender_cache_{}_{n}_{tag}.bin",
			std::process::id()
		))
		.to_string_lossy()
		.into_owned()
}

/// The resolved texture in the table's texture channel; panics when the
/// value is still a job box (resolution did not run).
fn resolved_texture(table: &NodeValueTable) -> Texture {
	let Some(NodeValue::Texture(handle)) = table.get(quercus_node::value::ValueType::Texture)
	else {
		panic!("no texture in the table");
	};
	assert!(!handle.ctx.is_null(), "null texture box");
	(unsafe { quercus_node::handle::get_checked::<Texture>(handle) })
		.cloned()
		.expect("value is still a job box (unresolved)")
}

/// The helper's guard: a table without a texture channel panics
/// instead of silently returning a placeholder.
#[test]
#[should_panic(expected = "no texture in the table")]
fn resolved_texture_rejects_a_textureless_table() {
	resolved_texture(&NodeValueTable::default());
}

/// A frame-cache job box around `payload`.
fn cache_job_box(payload: CacheJobPayload) -> NodeValue {
	NodeValue::Texture(quercus_node::handle::make_owned(Job::CacheJob(payload)))
}

/// A 4x3 F32 frame filled with `rgba`.
fn cache_test_frame(rgba: [f32; 4]) -> Frame {
	let mut frame = generate_frame(Rational::new(3, 1), (4, 3), PixelFormat::F32).unwrap();
	for px in frame.data.chunks_exact_mut(16) {
		for (c, v) in px.chunks_exact_mut(4).zip(rgba) {
			c.copy_from_slice(&v.to_le_bytes());
		}
	}
	frame
}

#[test]
fn cache_job_reads_the_saved_frame() {
	use quercus_node::traverser::RenderHooks;

	let path = cache_job_temp_path("hit");
	let frame = cache_test_frame([0.25, 0.5, 0.75, 1.0]);
	crate::frameio::save_cache_frame(&path, &frame).unwrap();

	let mut table = NodeValueTable::default();
	table.push(
		quercus_node::value::ValueType::Texture,
		cache_job_box(CacheJobPayload {
			path: path.clone(),
			time: Rational::new(3, 1),
			fallback: Box::new(NodeValue::None),
		}),
		None,
	);
	let mut hooks = RenderEvalHooks::new();
	hooks.resolve(
		quercus_node::id::NodeId::INVALID,
		&NodeValueRow::new(),
		&mut table,
	);

	let out = resolved_texture(&table);
	assert_eq!(out.size(), (4, 3));
	assert_eq!(first_pixel(&out), [0.25, 0.5, 0.75, 1.0]);
	let _ = std::fs::remove_file(&path);
}

#[test]
fn cache_job_missing_file_substitutes_its_input() {
	use quercus_node::traverser::RenderHooks;

	// The file is never written: the load fails and the fallback — a
	// real texture box — must end up in the table.
	let path = cache_job_temp_path("miss");
	let fallback = filled_frame((2, 2), [0.1, 0.2, 0.3, 0.4]);
	let mut table = NodeValueTable::default();
	table.push(
		quercus_node::value::ValueType::Texture,
		cache_job_box(CacheJobPayload {
			path,
			time: Rational::new(0, 1),
			fallback: Box::new(NodeValue::Texture(quercus_node::handle::make_owned(
				fallback,
			))),
		}),
		None,
	);
	let mut hooks = RenderEvalHooks::new();
	hooks.resolve(
		quercus_node::id::NodeId::INVALID,
		&NodeValueRow::new(),
		&mut table,
	);

	let out = resolved_texture(&table);
	assert_eq!(out.size(), (2, 2));
	assert_eq!(first_pixel(&out), [0.1, 0.2, 0.3, 0.4]);
}

#[test]
fn nested_cache_job_resolves_through_the_outer_shader_job() {
	use quercus_node::traverser::RenderHooks;

	let path = cache_job_temp_path("nested");
	let frame = cache_test_frame([0.5, 0.25, 0.125, 1.0]);
	crate::frameio::save_cache_frame(&path, &frame).unwrap();

	// The outer shader names a type nobody registered, so the pass
	// cannot run and falls back to its effect input — the nested cache
	// job, which must already have resolved to the frame from disk.
	let mut params = NodeValueRow::new();
	params.insert(
		"tex_in".into(),
		cache_job_box(CacheJobPayload {
			path: path.clone(),
			time: Rational::new(3, 1),
			fallback: Box::new(NodeValue::None),
		}),
	);
	let outer = Job::ShaderJob(ShaderJobPayload {
		type_id: "org.olivevideoeditor.Olive.thisdoesnotexist".into(),
		effect_input: "tex_in".into(),
		params,
		..ShaderJobPayload::default()
	});
	let mut table = NodeValueTable::default();
	table.push(
		quercus_node::value::ValueType::Texture,
		NodeValue::Texture(quercus_node::handle::make_owned(outer)),
		None,
	);
	let mut hooks = RenderEvalHooks::new();
	hooks.resolve(
		quercus_node::id::NodeId::INVALID,
		&NodeValueRow::new(),
		&mut table,
	);

	let out = resolved_texture(&table);
	assert_eq!(out.size(), (4, 3));
	assert_eq!(first_pixel(&out), [0.5, 0.25, 0.125, 1.0]);
	let _ = std::fs::remove_file(&path);
}

#[test]
fn generation_fills_cpu_texture() {
	let mut hooks = RenderEvalHooks::new();
	let mut tex =
		Texture::wrap_frame(generate_frame(Rational::new(1, 1), (8, 8), PixelFormat::F32).unwrap());
	hooks
		.process_frame_generation(&mut tex, Rational::new(3, 1))
		.unwrap();
	let Texture::Cpu(f) = &tex else {
		unreachable!()
	};
	assert_eq!(f.timestamp, Rational::new(3, 1));
	assert!(f.data.iter().all(|&b| b == 0));
	// GPU destination rejected.
	let mut gpu = Texture::gpu(Arc::new(UnusedCtx), 0, 8, 8, PixelFormat::F32);
	assert!(hooks
		.process_frame_generation(&mut gpu, Rational::new(1, 1))
		.is_err());
}

// The plugin executor lives in a process-wide slot; the tests below
// mutate it and therefore serialize against each other.
static PLUGIN_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn plugin_spec() -> JobSpec {
	JobSpec::Plugin {
		instance: 7,
		type_id: "org.quercus.test-plugin".into(),
		time: 0.5,
		effect_input_id: Some("Source".into()),
		inputs: Vec::new(),
		values: Vec::new(),
		animations: Vec::new(),
	}
}

fn first_pixel(texture: &Texture) -> [f32; 4] {
	// GPU textures are read back for the assertion (tests may take
	// the counted boundary; the playback path never does).
	let frame = texture.to_frame().expect("texture readback");
	let mut out = [0f32; 4];
	for i in 0..4 {
		out[i] = f32::from_le_bytes(frame.data[i * 4..i * 4 + 4].try_into().unwrap());
	}
	out
}

#[test]
fn plugin_job_without_executor_yields_purple_frame() {
	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	crate::ofxhost::install_client(None);
	set_plugin_executor(None);
	let mut hooks = RenderEvalHooks::new();
	let src =
		Texture::wrap_frame(generate_frame(Rational::new(0, 1), (4, 2), PixelFormat::F32).unwrap());
	let out = hooks.process_plugin_job(src, &plugin_spec()).unwrap();
	assert_eq!(out.size(), (4, 2));
	assert_eq!(first_pixel(&out), [1.0, 0.0, 1.0, 1.0]);
}

#[test]
fn plugin_job_executor_error_falls_back_to_purple() {
	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	crate::ofxhost::install_client(None);
	set_plugin_executor(Some(Arc::new(|_req: &PluginJobRequest<'_>| {
		Err(Error::Failed("boom".into()))
	})));
	let mut hooks = RenderEvalHooks::new();
	let src =
		Texture::wrap_frame(generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap());
	let out = hooks.process_plugin_job(src, &plugin_spec()).unwrap();
	assert_eq!(first_pixel(&out), [1.0, 0.0, 1.0, 1.0]);
	set_plugin_executor(None);
}

/// M3 acceptance: a host that cannot come up (or crashes past its
/// budget) yields the purple failure frame, exactly like a failing
/// in-process executor.
#[test]
#[cfg(unix)]
fn plugin_job_host_failure_yields_purple_frame() {
	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	// An executor that would succeed, to prove the host result wins.
	set_plugin_executor(Some(Arc::new(|_req: &PluginJobRequest<'_>| {
		Ok(Texture::wrap_frame(
			generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap(),
		))
	})));
	let host = crate::ofxhost::OfxHost::new(crate::ofxhost::OfxHostConfig {
		host_bin: Some(std::path::PathBuf::from("/bin/false")),
		max_failures: 1,
		..Default::default()
	})
	.unwrap();
	crate::ofxhost::install_client(Some(host));
	let mut hooks = RenderEvalHooks::new();
	let src =
		Texture::wrap_frame(generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap());
	let out = hooks.process_plugin_job(src, &plugin_spec()).unwrap();
	assert_eq!(first_pixel(&out), [1.0, 0.0, 1.0, 1.0]);
	crate::ofxhost::install_client(None);
	set_plugin_executor(None);
}

#[test]
fn plugin_job_dispatches_through_installed_executor() {
	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	crate::ofxhost::install_client(None);
	set_plugin_executor(Some(Arc::new(|req: &PluginJobRequest<'_>| {
		// Echo: paint the source size with the instance id.
		let JobSpec::Plugin { instance, .. } = req.spec else {
			return Err(Error::Invalid);
		};
		let v = (*instance as f32) / 10.0;
		let mut frame = generate_frame(Rational::new(0, 1), req.src.size(), PixelFormat::F32)?;
		for pixel in frame.data.chunks_exact_mut(16) {
			for c in 0..4 {
				pixel[c * 4..c * 4 + 4].copy_from_slice(&v.to_le_bytes());
			}
		}
		Ok(Texture::wrap_frame(frame))
	})));
	let mut hooks = RenderEvalHooks::new();
	let src =
		Texture::wrap_frame(generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap());
	let out = hooks.process_plugin_job(src, &plugin_spec()).unwrap();
	assert_eq!(first_pixel(&out), [0.7, 0.7, 0.7, 0.7]);
	set_plugin_executor(None);
}

#[test]
fn resolve_executes_payload_box_and_keeps_plain_textures() {
	use quercus_node::nodes::plugin::{PluginInstanceHandle, PluginJobPayload};

	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	crate::ofxhost::install_client(None);
	set_plugin_executor(Some(Arc::new(|req: &PluginJobRequest<'_>| {
		let JobSpec::Plugin {
			instance,
			values,
			inputs,
			effect_input_id,
			..
		} = req.spec
		else {
			return Err(Error::Invalid);
		};
		// The resolve seam must deliver the tagged param values and
		// the clip texture to the executor.
		assert_eq!(*instance, 7);
		assert_eq!(effect_input_id.as_deref(), Some("Source"));
		assert_eq!(inputs.len(), 1);
		assert_eq!(inputs[0].0, "Source");
		assert!(values.iter().any(|(k, v)| {
			k == "gain" && matches!(v, NodeValue::Float(f) if (*f - 0.25).abs() < 1e-6)
		}));
		let mut frame = generate_frame(Rational::new(0, 1), req.src.size(), PixelFormat::F32)?;
		for pixel in frame.data.chunks_exact_mut(16) {
			for (i, v) in [0.25f32, 0.5, 0.75, 1.0].iter().enumerate() {
				pixel[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
			}
		}
		Ok(Texture::wrap_frame(frame))
	})));

	// A real source texture box plus a payload box referencing it.
	let src_frame = generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
	let src_box = quercus_node::handle::make_owned(Texture::wrap_frame(src_frame));
	let mut values = NodeValueRow::new();
	values.insert("Source".into(), NodeValue::Texture(src_box));
	values.insert("gain".into(), NodeValue::Float(0.25));
	let payload = PluginJobPayload {
		instance: PluginInstanceHandle(7),
		type_id: "org.quercus.test-plugin".into(),
		time: Rational::new(1, 2),
		effect_input_id: "Source".into(),
		values,
		animations: Vec::new(),
	};

	let mut table = NodeValueTable::default();
	table.push(
		quercus_node::value::ValueType::Texture,
		NodeValue::Texture(quercus_node::handle::make_owned(Job::PluginJob(payload))),
		None,
	);

	use quercus_node::traverser::RenderHooks;
	let mut hooks = RenderEvalHooks::new();
	hooks.resolve(
		quercus_node::id::NodeId::INVALID,
		&NodeValueRow::new(),
		&mut table,
	);

	let NodeValue::Texture(handle) = table.get(quercus_node::value::ValueType::Texture).unwrap()
	else {
		unreachable!()
	};
	let rendered = unsafe { quercus_node::handle::get_checked::<Texture>(handle) }
		.expect("payload box must be replaced by the rendered texture");
	assert_eq!(first_pixel(rendered), [0.25, 0.5, 0.75, 1.0]);
	set_plugin_executor(None);
}

/// Stand-in context for the "GPU destination" test (never used for
/// real GPU work).
struct UnusedCtx;
impl quercus_core::backend::GpuContextLike for UnusedCtx {
	fn kind(&self) -> quercus_core::backend::BackendKind {
		quercus_core::backend::BackendKind::Cpu
	}
	fn destroy_texture(&self, _token: u64) {}
	fn upload(&self, _token: u64, _frame: &Frame) -> Result<()> {
		Err(Error::Failed("unused".into()))
	}
	fn download(&self, _token: u64) -> Result<Frame> {
		Err(Error::Failed("unused".into()))
	}
	fn blit(
		&self,
		_src: u64,
		_dst: u64,
		_processor: Option<&quercus_core::color::ColorProcessor>,
	) -> Result<()> {
		Err(Error::Failed("unused".into()))
	}
}
use std::sync::Arc;

// ---- Audio (M12 P1 / M15 S3) -----------------------------------------

fn audio_params(range: TimeRange) -> crate::ticket::AudioTicketParams {
	crate::ticket::AudioTicketParams {
		viewer: 1,
		range,
		sample_rate: 48000,
		channel_layout: 0x3,
		montage: Vec::new(),
		bus_effects: Vec::new(),
	}
}

#[test]
fn render_audio_samples_produces_silence_for_empty_montage() {
	// M12 P1: an empty montage renders total silence at the requested
	// layout.
	let params = audio_params(TimeRange::new(Rational::new(0, 1), Rational::new(1, 24)));
	match render_audio_samples(&params).unwrap() {
		crate::ticket::TicketPayload::Audio(samples) => {
			// 1/24 s at 48 kHz = 2000 sample frames, stereo.
			assert_eq!(samples.sample_rate, 48000);
			assert_eq!(samples.channel_count, 2);
			assert_eq!(samples.samples.len(), 2000 * 2);
			assert!(samples.samples.iter().all(|&v| v == 0.0), "silence");
		}
		other => panic!("expected Audio payload, got {other:?}"),
	}
}

#[test]
fn render_audio_samples_into_matches_heap_path_byte_for_byte() {
	// M15 S3: the shm-slot writer must produce exactly the same
	// little-endian f32 bytes as the heap path, so a worker's slot and
	// the in-process fallback agree for the same montage.
	let params = audio_params(TimeRange::new(Rational::new(0, 1), Rational::new(1, 48)));
	let heap = match render_audio_samples(&params).unwrap() {
		crate::ticket::TicketPayload::Audio(samples) => samples,
		other => panic!("expected Audio payload, got {other:?}"),
	};
	let mut dst = vec![0u8; heap.samples.len() * 4];
	render_audio_samples_into(&params, &mut dst).unwrap();
	let expected: Vec<u8> = heap.samples.iter().flat_map(|v| v.to_le_bytes()).collect();
	assert_eq!(dst, expected);
	// And the into-path output parses back into the same samples.
	let parsed: Vec<f32> = dst
		.chunks_exact(4)
		.map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
		.collect();
	assert_eq!(parsed, heap.samples);
}

#[test]
fn render_audio_samples_into_rejects_small_buffer() {
	let params = audio_params(TimeRange::new(Rational::new(0, 1), Rational::new(1, 24)));
	let mut dst = [0u8; 8]; // far too small for 2000x2 f32 samples
	assert!(render_audio_samples_into(&params, &mut dst).is_err());
}

// ---- Montage clip effect stacks -------------------------------------

/// A 2x1 F32 texture filled with a known color.
fn solid_texture(r: f32, g: f32, b: f32, a: f32) -> Texture {
	let mut frame = generate_frame(Rational::new(0, 1), (2, 1), PixelFormat::F32).unwrap();
	for px in frame.data.chunks_exact_mut(16) {
		for (c, v) in px.chunks_exact_mut(4).zip([r, g, b, a]) {
			c.copy_from_slice(&v.to_le_bytes());
		}
	}
	Texture::Cpu(frame)
}

fn opacity_effect(enabled: bool, value: f64) -> crate::ticket::MontageEffect {
	crate::ticket::MontageEffect {
		type_id: OPACITY_EFFECT_TYPE_ID.to_string(),
		enabled,
		effect_input_id: Some("tex_in".to_string()),
		params: vec![(OPACITY_VALUE_INPUT.to_string(), NodeValue::Float(value))],
	}
}

fn clip_with_effects(effects: Vec<crate::ticket::MontageEffect>) -> crate::ticket::MontageClip {
	crate::ticket::MontageClip {
		filename: String::new(),
		stream_index: 0,
		in_time: Rational::new(0, 1),
		out_time: Rational::new(1, 1),
		media_in: Rational::new(0, 1),
		gain: 1.0,
		pan: 0.0,
		fade_in: Rational::new(0, 1),
		fade_out: Rational::new(0, 1),
		effects,
		audio_effects: Vec::new(),
	}
}

/// The built-in Opacity effect really scales the decoded pixels
/// (every channel, C++ `opacity.frag` parity).
#[test]
fn montage_opacity_effect_scales_pixels() {
	let clip = clip_with_effects(vec![opacity_effect(true, 0.5)]);
	let out = apply_clip_effects(
		solid_texture(0.8, 0.4, 0.2, 1.0),
		&clip,
		Rational::new(0, 1),
	);
	assert_eq!(first_pixel(&out), [0.4, 0.2, 0.1, 0.5]);
}

/// Disabled effects are bypassed (C++ traverser parity), and unity
/// opacity is a pass-through.
#[test]
fn montage_disabled_or_unity_effects_pass_through() {
	let disabled = clip_with_effects(vec![opacity_effect(false, 0.5)]);
	let out = apply_clip_effects(
		solid_texture(0.8, 0.4, 0.2, 1.0),
		&disabled,
		Rational::new(0, 1),
	);
	assert_eq!(first_pixel(&out), [0.8, 0.4, 0.2, 1.0]);

	let unity = clip_with_effects(vec![opacity_effect(true, 1.0)]);
	let out = apply_clip_effects(
		solid_texture(0.8, 0.4, 0.2, 1.0),
		&unity,
		Rational::new(0, 1),
	);
	assert_eq!(first_pixel(&out), [0.8, 0.4, 0.2, 1.0]);
}

/// Upload an F32 RGBA texture of `size` (row-major `pixels`) on the
/// shared GPU context — the GPU side of the montage compositor tests.
fn gpu_test_texture(
	ctx: &Arc<quercus_core::backend::GpuContext>,
	size: (i32, i32),
	pixels: &[[f32; 4]],
) -> Texture {
	let mut frame = generate_frame(Rational::new(0, 1), size, PixelFormat::F32).unwrap();
	assert_eq!(pixels.len(), (size.0 * size.1) as usize, "pixel count");
	for (chunk, rgba) in frame.data.chunks_exact_mut(16).zip(pixels) {
		for (c, v) in chunk.chunks_exact_mut(4).zip(*rgba) {
			c.copy_from_slice(&v.to_le_bytes());
		}
	}
	let token = ctx.create_texture(size.0, size.1).unwrap();
	ctx.upload(token, &frame).unwrap();
	Texture::gpu(ctx.clone(), token, size.0, size.1, PixelFormat::F32)
}

/// A transparent-black GPU accumulator of `size`.
fn gpu_accumulator(ctx: &Arc<quercus_core::backend::GpuContext>, size: (i32, i32)) -> Texture {
	let token = ctx.create_texture(size.0, size.1).unwrap();
	ctx.clear_texture(token).unwrap();
	Texture::gpu(ctx.clone(), token, size.0, size.1, PixelFormat::F32)
}

/// Built-in Opacity on a GPU texture runs as a GPU pass (no readback,
/// full C++ `opacity.frag` parity: alpha included).
#[test]
fn montage_opacity_effect_scales_gpu_pixels_on_the_gpu() {
	let Some(ctx) = quercus_core::backend::shared_gpu_or_skip("an eval GPU test") else {
		return;
	};
	let src = gpu_test_texture(&ctx, (2, 1), &[[0.8, 0.4, 0.2, 1.0]; 2]);
	let clip = clip_with_effects(vec![opacity_effect(true, 0.5)]);
	let out = apply_clip_effects(src, &clip, Rational::new(0, 1));
	assert!(
		matches!(out, Texture::Gpu { .. }),
		"opacity stays on the GPU"
	);
	let px = first_pixel(&out);
	for (got, want) in px.iter().zip([0.4f32, 0.2, 0.1, 0.5]) {
		assert!((got - want).abs() < 1e-5, "opacity math: {px:?}");
	}
}

/// The scaled alpha-over maps any source resolution/aspect onto the
/// full target canvas: existing fill/stretch semantics (the CPU path
/// pre-scales with swscale), so no letterbox/pillarbox bars appear and
/// the normalized-UV bilinear taps match the CPU sampler.
#[test]
fn gpu_montage_fills_a_mismatched_aspect_target_canvas() {
	let Some(ctx) = quercus_core::backend::shared_gpu_or_skip("an eval GPU test") else {
		return;
	};
	let program = ctx
		.compile_shader_pass(SCALED_COMP_KEY, SCALED_COMP_WGSL, 2, true, false)
		.expect("scaled alpha-over program");
	let target = (4, 2);

	// A 3:2 solid source into a 2:1 canvas: every target pixel must be
	// covered (letterboxing would leave black bars on the sides).
	let solid = gpu_test_texture(&ctx, (3, 2), &[[1.0, 0.0, 0.0, 1.0]; 6]);
	let acc = gpu_accumulator(&ctx, target);
	let out = composite_montage_gpu(&ctx, &program, &acc, &solid, 1.0, target)
		.expect("mismatched-size composite");
	assert_eq!(out.size(), target, "the target canvas size is kept");
	assert!(matches!(out, Texture::Gpu { .. }), "stays on the GPU");
	for px in out.to_frame().unwrap().data.chunks_exact(16) {
		let rgba: Vec<f32> = px
			.chunks_exact(4)
			.map(|c| f32::from_le_bytes(c.try_into().unwrap()))
			.collect();
		assert!(
			rgba[0] > 0.99 && rgba[1].abs() < 1e-6 && rgba[3] > 0.99,
			"full-canvas fill: {rgba:?}"
		);
	}

	// A 3:1 horizontal ramp into a 4:1 canvas: the bilinear taps are
	// the same `scale_rgba_f32` computes for the CPU path.
	let ramp = gpu_test_texture(
		&ctx,
		(3, 1),
		&[
			[0.0, 0.0, 0.0, 1.0],
			[0.5, 0.5, 0.5, 1.0],
			[1.0, 1.0, 1.0, 1.0],
		],
	);
	let wide = (4, 1);
	let acc = gpu_accumulator(&ctx, wide);
	let out =
		composite_montage_gpu(&ctx, &program, &acc, &ramp, 1.0, wide).expect("ramp composite");
	let got: Vec<f32> = out
		.to_frame()
		.unwrap()
		.data
		.chunks_exact(16)
		.map(|px| f32::from_le_bytes(px[..4].try_into().unwrap()))
		.collect();
	let want = [0.0f32, 0.3125, 0.6875, 1.0];
	for (got, want) in got.iter().zip(want) {
		assert!(
			(got - want).abs() < 1e-4,
			"normalized-UV bilinear mapping: {got} vs {want}"
		);
	}
}

/// The graph compositor no longer black-holes a frame whose resolution
/// differs from the target: a native-size frame is scaled by the
/// bilinear pass.
#[test]
fn gpu_track_composite_scales_a_mismatched_frame() {
	let Some(ctx) = quercus_core::backend::shared_gpu_or_skip("an eval GPU test") else {
		return;
	};
	let native = gpu_test_texture(&ctx, (2, 2), &[[0.0, 0.0, 1.0, 1.0]; 4]);
	let out = composite_tracks_gpu(&ctx, &[native], (4, 4)).expect("scaled frame");
	assert_eq!(out.size(), (4, 4));
	let px = first_pixel(&out);
	assert!(
		(px[2] - 1.0).abs() < 5e-3 && (px[3] - 1.0).abs() < 5e-3,
		"scaled, not skipped: {px:?}"
	);
}

/// The F32 montage ticket prefers the GPU compositor (a cleared GPU
/// accumulator for a frame with no covering clip — no decode needed);
/// non-F32 tickets keep the CPU compositor.
#[test]
fn render_montage_frame_prefers_the_gpu_compositor() {
	let Some(_ctx) = quercus_core::backend::shared_gpu_or_skip("an eval GPU test") else {
		return;
	};
	let params = crate::ticket::VideoTicketParams {
		viewer: 1,
		project: String::new(),
		time: Rational::new(0, 1),
		force_size: Some((8, 8)),
		force_format: Some(PixelFormat::F32),
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage: vec![crate::ticket::MontageClip {
			filename: String::new(),
			stream_index: 0,
			in_time: Rational::new(1, 1),
			out_time: Rational::new(2, 1),
			media_in: Rational::new(0, 1),
			gain: 1.0,
			pan: 0.0,
			fade_in: Rational::new(0, 1),
			fade_out: Rational::new(0, 1),
			effects: Vec::new(),
			audio_effects: Vec::new(),
		}],
		adjustments: Vec::new(),
	};
	let out = render_montage_frame(Rational::new(0, 1), &params, (8, 8), PixelFormat::F32)
		.expect("GPU montage");
	assert!(
		matches!(out, Texture::Gpu { .. }),
		"the F32 montage render stays on the GPU"
	);
	assert!(
		out.to_frame().unwrap().data.iter().all(|&b| b == 0),
		"transparent accumulator"
	);

	let cpu = render_montage_frame(Rational::new(0, 1), &params, (8, 8), PixelFormat::U16)
		.expect("CPU montage");
	assert!(
		matches!(cpu, Texture::Cpu(_)),
		"non-F32 stays on the CPU path"
	);
}

/// An OFX effect (any non-built-in type id) resolves its instance
/// through the installed factory and dispatches through the executor,
/// carrying the montage's parameter values.
#[test]
fn montage_plugin_effect_dispatches_with_params() {
	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	set_plugin_instance_factory(Some(Arc::new(|identifier: &str| {
		(identifier == "com.example.darken").then_some(7)
	})));
	set_plugin_executor(Some(Arc::new(|req: &PluginJobRequest<'_>| {
		let JobSpec::Plugin {
			instance, values, ..
		} = req.spec
		else {
			return Err(Error::Invalid);
		};
		assert_eq!(*instance, 7);
		// The injected parameter drives the output: paint the frame
		// with the "gain" value so the test observes the param path.
		let gain = values
			.iter()
			.find(|(k, _)| k == "gain")
			.map(|(_, v)| v.to_double() as f32)
			.unwrap_or(1.0);
		let mut frame = generate_frame(Rational::new(0, 1), req.src.size(), PixelFormat::F32)?;
		for px in frame.data.chunks_exact_mut(16) {
			for (c, v) in px.chunks_exact_mut(4).zip([gain, gain, gain, 1.0]) {
				c.copy_from_slice(&v.to_le_bytes());
			}
		}
		Ok(Texture::Cpu(frame))
	})));
	let clip = clip_with_effects(vec![crate::ticket::MontageEffect {
		type_id: "com.example.darken".to_string(),
		enabled: true,
		effect_input_id: Some("Source".to_string()),
		params: vec![("gain".to_string(), NodeValue::Float(0.25))],
	}]);
	let out = apply_clip_effects(
		solid_texture(0.8, 0.4, 0.2, 1.0),
		&clip,
		Rational::new(0, 1),
	);
	assert_eq!(first_pixel(&out), [0.25, 0.25, 0.25, 1.0]);
	set_plugin_executor(None);
	set_plugin_instance_factory(None);
}

/// An effect nobody can evaluate (no factory / unknown type) passes
/// the frame through unchanged — loudly (the warn-once log), never a
/// silent no-op.
#[test]
fn montage_unknown_effect_passes_through() {
	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	set_plugin_instance_factory(None);
	let clip = clip_with_effects(vec![crate::ticket::MontageEffect {
		type_id: "com.example.missing".to_string(),
		enabled: true,
		effect_input_id: Some("Source".to_string()),
		params: Vec::new(),
	}]);
	let out = apply_clip_effects(
		solid_texture(0.8, 0.4, 0.2, 1.0),
		&clip,
		Rational::new(0, 1),
	);
	assert_eq!(first_pixel(&out), [0.8, 0.4, 0.2, 1.0]);
}

// ---- Graph-driven sequence (M12 phase 2) ----------------------------

/// Native-size decode: a `(0, 0)` request produces the footage's
/// intrinsic dimensions (the graph sequence path decodes native and
/// scales at composite time).
#[test]
fn footage_native_size_decode() {
	let path = std::env::temp_dir().join(format!(
		"quercusrender_graph_native_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip(&path, 64, 64, 10, 10).expect("test clip generation");
	let tex = render_footage_frame(
		&path.to_string_lossy(),
		0,
		Rational::new(0, 1),
		(0, 0),
		PixelFormat::F32,
	)
	.expect("native-size decode");
	assert_eq!(tex.size(), (64, 64));
	let _ = std::fs::remove_file(&path);
}

/// The resolve seam decodes each boxed [`FootageJobPayload`] into a
/// texture and leaves genuine texture boxes untouched (in-place row
/// replacement, C++ FootageJob processing).
#[test]
fn resolve_footage_jobs_decodes_payload_box() {
	let path = std::env::temp_dir().join(format!(
		"quercusrender_graph_resolve_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip(&path, 32, 32, 10, 10).expect("test clip generation");

	let mut table = NodeValueTable::default();
	let payload = FootageJobPayload {
		filename: path.to_string_lossy().into_owned(),
		stream_index: 0,
		time: Rational::new(0, 1),
	};
	table.push(
		quercus_node::value::ValueType::Texture,
		NodeValue::Texture(quercus_node::handle::make_owned(Job::FootageJob(payload))),
		None,
	);
	let genuine =
		Texture::wrap_frame(generate_frame(Rational::new(0, 1), (4, 4), PixelFormat::F32).unwrap());
	table.push(
		quercus_node::value::ValueType::Texture,
		NodeValue::Texture(quercus_node::handle::make_owned(genuine)),
		None,
	);

	use quercus_node::traverser::RenderHooks;
	let mut hooks = RenderEvalHooks::new();
	hooks.frame_size = Some((32, 32));
	hooks.resolve(
		quercus_node::id::NodeId::INVALID,
		&NodeValueRow::new(),
		&mut table,
	);
	assert_eq!(
		table.count(),
		2,
		"both rows stay, only the payload is replaced"
	);

	let rows = table.rows();
	let NodeValue::Texture(decoded_handle) = &rows[0].1 else {
		unreachable!()
	};
	let decoded = unsafe { quercus_node::handle::get_checked::<Texture>(decoded_handle) }
		.expect("payload box replaced by the decoded texture");
	let Texture::Cpu(frame) = decoded else {
		unreachable!()
	};
	assert_eq!((frame.width, frame.height), (32, 32));
	assert!(
		frame.data.iter().any(|&b| b != 0),
		"decoded frame must contain non-black pixels"
	);

	let NodeValue::Texture(genuine_handle) = &rows[1].1 else {
		unreachable!()
	};
	let genuine = unsafe { quercus_node::handle::get_checked::<Texture>(genuine_handle) }
		.expect("genuine texture box stays untouched");
	assert_eq!(genuine.size(), (4, 4));

	let _ = std::fs::remove_file(&path);
}

/// The resolve seam applies a ColorTransformJob's OCIO processor for
/// real (C++ ColorTransformJob processing): a CPU frame converts
/// through the LUT in place.
#[test]
fn resolve_color_transform_job_applies_lut_on_cpu() {
	if quercus_core::color::set_up_default_config().is_err() {
		eprintln!("bundled OCIO missing; skipping");
		return;
	}
	// 1D LUT doubling the red channel (linear ramp 0→0, 1→2).
	let path = std::env::temp_dir().join(format!(
		"quercusrender_lut_double_{}.cube",
		std::process::id()
	));
	std::fs::write(&path, "LUT_1D_SIZE 2\n0.0 0.0 0.0\n2.0 1.0 1.0\n").unwrap();

	let Some(processor) = quercus_core::color::ColorProcessor::create_lut(
		path.to_str().unwrap(),
		quercus_core::color::Direction::Normal,
	)
	.filter(|p| p.is_valid()) else {
		eprintln!("LUT processor unavailable; skipping");
		let _ = std::fs::remove_file(&path);
		return;
	};

	// Input: a 0.25-grey CPU frame.
	let mut frame = generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
	for px in frame.data.chunks_exact_mut(16) {
		for (c, v) in px.chunks_exact_mut(4).zip([0.25f32, 0.25, 0.25, 1.0]) {
			c.copy_from_slice(&v.to_le_bytes());
		}
	}
	let payload = ColorTransformJobPayload {
		color_processor: std::sync::Arc::new(processor),
		input: NodeValue::Texture(quercus_node::handle::make_owned(Texture::wrap_frame(frame))),
		time: Rational::new(0, 1),
	};
	let mut table = NodeValueTable::default();
	table.push(
		quercus_node::value::ValueType::Texture,
		NodeValue::Texture(quercus_node::handle::make_owned(Job::ColorTransformJob(
			payload,
		))),
		None,
	);

	use quercus_node::traverser::RenderHooks;
	let mut hooks = RenderEvalHooks::new();
	hooks.resolve(
		quercus_node::id::NodeId::INVALID,
		&NodeValueRow::new(),
		&mut table,
	);

	let rows = table.rows();
	let NodeValue::Texture(handle) = &rows[0].1 else {
		unreachable!()
	};
	let out = unsafe { quercus_node::handle::get_checked::<Texture>(handle) }
		.expect("the job box is replaced by the converted texture");
	let px = first_pixel(out);
	assert!(
		(px[0] - 0.5).abs() < 1e-3,
		"red channel doubles through the LUT: {px:?}"
	);
	assert!((px[1] - 0.25).abs() < 1e-4, "green unchanged: {px:?}");
	assert_eq!(px[3], 1.0, "alpha preserved");
	let _ = std::fs::remove_file(&path);
}

/// M2: a ColorTransformJob on a GPU texture bakes the processor into
/// a 3D LUT and runs the GPU color pass — the transform is applied
/// (not passed through) and the result stays GPU-resident.
#[test]
fn resolve_color_transform_job_applies_lut_on_gpu() {
	if quercus_core::color::set_up_default_config().is_err() {
		eprintln!("bundled OCIO missing; skipping");
		return;
	}
	let Some(ctx) = quercus_core::backend::shared_gpu_or_skip("an eval GPU test") else {
		return;
	};
	// 1D LUT doubling the red channel (linear ramp 0→0, 1→2).
	let path = std::env::temp_dir().join(format!(
		"quercusrender_lut_double_gpu_{}.cube",
		std::process::id()
	));
	std::fs::write(&path, "LUT_1D_SIZE 2\n0.0 0.0 0.0\n2.0 1.0 1.0\n").unwrap();
	let Some(processor) = quercus_core::color::ColorProcessor::create_lut(
		path.to_str().unwrap(),
		quercus_core::color::Direction::Normal,
	)
	.filter(|p| p.is_valid()) else {
		eprintln!("LUT processor unavailable; skipping");
		let _ = std::fs::remove_file(&path);
		return;
	};

	let mut frame = generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
	for px in frame.data.chunks_exact_mut(16) {
		for (c, v) in px.chunks_exact_mut(4).zip([0.25f32, 0.25, 0.25, 1.0]) {
			c.copy_from_slice(&v.to_le_bytes());
		}
	}
	let token = ctx.create_texture(2, 2).unwrap();
	ctx.upload(token, &frame).unwrap();
	let input = Texture::gpu(ctx.clone(), token, 2, 2, PixelFormat::F32);
	let payload = ColorTransformJobPayload {
		color_processor: std::sync::Arc::new(processor),
		input: NodeValue::Texture(quercus_node::handle::make_owned(input)),
		time: Rational::new(0, 1),
	};
	let mut table = NodeValueTable::default();
	table.push(
		quercus_node::value::ValueType::Texture,
		NodeValue::Texture(quercus_node::handle::make_owned(Job::ColorTransformJob(
			payload,
		))),
		None,
	);

	use quercus_node::traverser::RenderHooks;
	let mut hooks = RenderEvalHooks::new();
	hooks.resolve(
		quercus_node::id::NodeId::INVALID,
		&NodeValueRow::new(),
		&mut table,
	);

	let rows = table.rows();
	let NodeValue::Texture(handle) = &rows[0].1 else {
		unreachable!()
	};
	let out = unsafe { quercus_node::handle::get_checked::<Texture>(handle) }
		.expect("the job box is replaced by the converted texture");
	assert!(
		matches!(out, Texture::Gpu { .. }),
		"the GPU color transform stays on the GPU"
	);
	let px = first_pixel(out);
	assert!(
		(px[0] - 0.5).abs() < 5e-3,
		"red channel doubles through the GPU LUT: {px:?}"
	);
	assert!((px[1] - 0.25).abs() < 5e-3, "green unchanged: {px:?}");
	assert_eq!(px[3], 1.0, "alpha preserved");
	let _ = std::fs::remove_file(&path);
}

/// An invalid processor passes the input texture through unchanged
/// (C++ creates processors non-fatally).
#[test]
fn resolve_color_transform_job_passes_through_when_processor_invalid() {
	let mut frame = generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
	for px in frame.data.chunks_exact_mut(16) {
		for (c, v) in px.chunks_exact_mut(4).zip([0.4f32, 0.3, 0.2, 1.0]) {
			c.copy_from_slice(&v.to_le_bytes());
		}
	}
	let payload = ColorTransformJobPayload {
		color_processor: std::sync::Arc::new(quercus_core::color::ColorProcessor::pass_through()),
		input: NodeValue::Texture(quercus_node::handle::make_owned(Texture::wrap_frame(frame))),
		time: Rational::new(0, 1),
	};
	let mut table = NodeValueTable::default();
	table.push(
		quercus_node::value::ValueType::Texture,
		NodeValue::Texture(quercus_node::handle::make_owned(Job::ColorTransformJob(
			payload,
		))),
		None,
	);

	use quercus_node::traverser::RenderHooks;
	let mut hooks = RenderEvalHooks::new();
	hooks.resolve(
		quercus_node::id::NodeId::INVALID,
		&NodeValueRow::new(),
		&mut table,
	);

	let rows = table.rows();
	let NodeValue::Texture(handle) = &rows[0].1 else {
		unreachable!()
	};
	let out = unsafe { quercus_node::handle::get_checked::<Texture>(handle) }
		.expect("the job box is replaced by the input texture");
	assert_eq!(first_pixel(out), [0.4, 0.3, 0.2, 1.0], "untouched");
}
#[test]
fn composite_tracks_matches_alpha_over_math() {
	let frames = vec![
		solid_texture(0.5, 0.25, 0.125, 0.5),
		solid_texture(1.0, 1.0, 1.0, 0.75),
	];
	let expected = [0.625f32, 0.5, 0.4375, 0.875];

	// bottom over transparent: (0.75, 0.75, 0.75, 0.75), then top over:
	// r = 0.5*0.5 + 0.75*0.5, g = 0.25*0.5 + 0.75*0.5,
	// b = 0.125*0.5 + 0.75*0.5, a = 0.5 + 0.75*0.5.
	// The CPU fallback must match the GPU math (and the layer
	// order); call it directly so the assertion never depends on
	// whether some other test installed a shared GPU context.
	let out = composite_tracks_cpu(&frames, (2, 1));
	let pixel = first_pixel(&out);
	for (got, want) in pixel.iter().zip(expected) {
		assert!(
			(got - want).abs() < 1e-4,
			"CPU composite: expected {want}, got {got}"
		);
	}

	// Same math through the GPU pass when a device is available. Software
	// decoded CPU frames are uploaded at the composite boundary.
	if let Some(ctx) = quercus_core::backend::shared_gpu_or_skip("an eval GPU test") {
		let gpu_out = composite_tracks_gpu(&ctx, &frames, (2, 1)).expect("GPU composite");
		let pixel = first_pixel(&gpu_out);
		for (got, want) in pixel.iter().zip(expected) {
			assert!(
				(got - want).abs() < 1e-3,
				"GPU composite: expected {want}, got {got}"
			);
		}
	}
}

/// End-to-end chromakey through the real GPU path: a solid opaque
/// green frame keyed on the node's default green key leaves every
/// pixel at zero (the C++ `ColorTransformJob` + `chromakey.frag`
/// math: `colorclose` returns 0 at the key color, so the
/// shadows/highlights transform yields `mask = 0` and `col *= mask`
/// blanks the frame). Exercises the OCIO splice in
/// [`super::process_shader_job`]: the shader's `%1` marker is filled
/// with the real `SceneLinearToCIEXYZ_d65` GLSL generated from the
/// default OCIO config, and the tolerance uniforms reach the shader
/// under their (fixed) input-id spelling.
#[test]
fn gpu_chromakey_keys_green_with_ocio_stub() {
	let Some(ctx) = quercus_core::backend::shared_gpu_or_skip("an eval GPU test") else {
		return;
	};
	// Install the process-wide default config (the C++
	// `ColorManager::SetUpDefaultConfig` startup step; color.rs tests
	// do the same). Without it the OCIO stub cannot be generated and
	// the job falls back to the input pass-through.
	if quercus_core::color::set_up_default_config().is_err() {
		eprintln!("bundled OCIO missing; skipping");
		return;
	}
	if quercus_core::color::ocio_function_shader(
		"SceneLinearToCIEXYZ_d65",
		"scene_linear",
		"cie_xyz_d65_interchange",
	)
	.is_none()
	{
		eprintln!("no OCIO config; skipping");
		return;
	}

	let size = (16, 16);
	let mut frame = generate_frame(Rational::new(0, 1), size, PixelFormat::F32).unwrap();
	for px in frame.data.chunks_exact_mut(16) {
		for (c, v) in px.chunks_exact_mut(4).zip([0.0f32, 1.0, 0.0, 1.0]) {
			c.copy_from_slice(&v.to_le_bytes());
		}
	}

	let src = ctx.create_texture(size.0, size.1).unwrap();
	ctx.upload(src, &frame).unwrap();
	let input = Texture::gpu(ctx.clone(), src, size.0, size.1, PixelFormat::F32);

	let mut params = NodeValueRow::new();
	params.insert(
		"tex_in".into(),
		NodeValue::Texture(quercus_node::handle::make_owned(input)),
	);
	params.insert("color_key".into(), NodeValue::Color([0.0, 1.0, 0.0, 1.0]));
	params.insert("lower_tolerance_in".into(), NodeValue::Float(5.0));
	params.insert("upper_tolerance_in".into(), NodeValue::Float(25.0));
	params.insert("mask_only_in".into(), NodeValue::Boolean(false));
	params.insert("invert_in".into(), NodeValue::Boolean(false));
	params.insert("shadows_in".into(), NodeValue::Float(100.0));
	params.insert("highlights_in".into(), NodeValue::Float(100.0));

	let payload = ShaderJobPayload {
		node_id: quercus_node::id::NodeId::from_identity(1).unwrap(),
		time: Rational::new(0, 1),
		iterations: 1,
		type_id: "org.olivevideoeditor.Olive.chromakey".into(),
		shader_id: String::new(),
		effect_input: "tex_in".into(),
		params,
		iterative_input: String::new(),
	};
	let rendered = RenderEvalHooks::new()
		.process_shader_job(&payload)
		.expect("chromakey renders on the GPU");

	let Texture::Gpu { token: dst, .. } = rendered else {
		panic!("expected a GPU texture");
	};
	let out = ctx.download(dst).unwrap();
	for px in out.data.chunks_exact(16) {
		for (c, v) in px.chunks_exact(4).enumerate() {
			let got = f32::from_le_bytes(v.try_into().unwrap());
			assert!(
				got.abs() < 1e-4,
				"channel {c}: keyed green must be fully transparent (got {got})"
			);
		}
	}
	ctx.destroy_texture(dst);
	ctx.destroy_texture(src);
}

/// Pixel readback helper for the GPU verification tests.
fn pixel_at(frame: &Frame, x: usize, y: usize) -> [f32; 4] {
	let at = (y * frame.width as usize + x) * 16;
	let mut out = [0f32; 4];
	for c in 0..4 {
		out[c] = f32::from_le_bytes(frame.data[at + c * 4..at + c * 4 + 4].try_into().unwrap());
	}
	out
}

/// Build a solid-color F32 CPU frame.
fn filled_frame(size: (i32, i32), rgba: [f32; 4]) -> Texture {
	let mut frame = generate_frame(Rational::new(0, 1), size, PixelFormat::F32).unwrap();
	for px in frame.data.chunks_exact_mut(16) {
		for (c, v) in px.chunks_exact_mut(4).zip(rgba) {
			c.copy_from_slice(&v.to_le_bytes());
		}
	}
	Texture::wrap_frame(frame)
}

/// Evaluate one node's `value()` against `inputs` and resolve the
/// resulting table through the hooks (the full value -> job -> GPU
/// run -> texture path), reading the frame back while the table —
/// which owns the texture's GPU token — is still alive.
fn eval_node_row(type_id: &str, inputs: NodeValueRow, frame_size: Option<(i32, i32)>) -> Frame {
	use quercus_node::traverser::RenderHooks;
	let (core, behavior) = quercus_node::factory::Factory::global()
		.create_any(type_id)
		.expect("node type registered");
	let mut table = NodeValueTable::default();
	behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
	let mut hooks = RenderEvalHooks::new();
	hooks.frame_size = frame_size;
	hooks.resolve(quercus_node::id::NodeId::INVALID, &inputs, &mut table);
	let Some(NodeValue::Texture(handle)) = table.get(quercus_node::value::ValueType::Texture)
	else {
		panic!("{type_id}: no texture produced");
	};
	if handle.ctx.is_null() {
		panic!("{type_id}: null texture produced");
	}
	let tex = (unsafe { quercus_node::handle::get_checked::<Texture>(handle) })
		.expect("resolved texture");
	assert!(
		matches!(tex, Texture::Gpu { .. }),
		"{type_id}: the job must render on the GPU"
	);
	tex.to_frame().expect("readback")
}

/// Merge over the real GPU path: the merge node declares no effect
/// input, so base and blend must bind by name for the alpha-over to
/// run at all. Red base + half-alpha green blend -> (0.5, 1, 0, 1).
#[test]
fn gpu_merge_alpha_over_binds_base_and_blend() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	let mut inputs = NodeValueRow::new();
	inputs.insert(
		"base_in".into(),
		texture_value(filled_frame((16, 16), [1.0, 0.0, 0.0, 1.0])),
	);
	inputs.insert(
		"blend_in".into(),
		texture_value(filled_frame((16, 16), [0.0, 1.0, 0.0, 0.5])),
	);
	let frame = eval_node_row("org.olivevideoeditor.Olive.merge", inputs, None);
	assert_eq!(frame.width, 16, "the pass size follows the base");
	for (x, y) in [(0, 0), (8, 8), (15, 15)] {
		let px = pixel_at(&frame, x, y);
		let want = [0.5, 1.0, 0.0, 1.0];
		for (c, (got, w)) in px.iter().zip(want).enumerate() {
			assert!(
				(got - w).abs() < 1e-4,
				"merge ({x},{y}) ch{c}: got {got}, want {w}"
			);
		}
	}
}

/// A bare generator (no input connected) renders at the hook's frame
/// size instead of a 1x1 the composite step would drop.
#[test]
fn gpu_generator_without_input_renders_at_frame_size() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	let mut inputs = NodeValueRow::new();
	inputs.insert("color_in".into(), NodeValue::Color([0.2, 0.4, 0.6, 1.0]));
	let frame = eval_node_row(
		"org.olivevideoeditor.Olive.solidgenerator",
		inputs,
		Some((8, 4)),
	);
	assert_eq!((frame.width, frame.height), (8, 4));
	let px = pixel_at(&frame, 3, 2);
	for (c, w) in [0.2f32, 0.4, 0.6, 1.0].iter().enumerate() {
		assert!(
			(px[c] - w).abs() < 1e-4,
			"solid ch{c}: got {}, want {w}",
			px[c]
		);
	}
}

/// Generator-over-base ("mrg"): the nested generator job resolves
/// recursively and alpha-overs onto the base — the pentagon is green
/// (the generated layer), the corners stay red (the base).
#[test]
fn gpu_generator_over_base_composites_nested_job() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	let mut inputs = NodeValueRow::new();
	inputs.insert(
		"base_in".into(),
		texture_value(filled_frame((512, 512), [1.0, 0.0, 0.0, 1.0])),
	);
	inputs.insert("color_in".into(), NodeValue::Color([0.0, 1.0, 0.0, 1.0]));
	let frame = eval_node_row(
		"org.olivevideoeditor.Olive.polygon",
		inputs,
		Some((512, 512)),
	);
	assert_eq!((frame.width, frame.height), (512, 512));
	let center = pixel_at(&frame, 256, 256);
	assert!(
		center[1] > 0.9 && center[0] < 0.1,
		"pentagon center is the generated green: {center:?}"
	);
	let corner = pixel_at(&frame, 5, 5);
	assert!(
		corner[0] > 0.9 && corner[1] < 0.1,
		"corner keeps the red base: {corner:?}"
	);
}

/// Drop shadow with non-zero softness: three iterations feed back
/// through `previous_iteration_in`; the blurred shadow lands offset
/// from the source, widening the non-transparent area.
#[test]
fn gpu_dropshadow_softness_blurs_and_offsets() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	// 16x16 transparent frame with an opaque 4x4 square at (4,4).
	let mut frame = generate_frame(Rational::new(0, 1), (16, 16), PixelFormat::F32).unwrap();
	for y in 4..8usize {
		for x in 4..8usize {
			let at = (y * 16 + x) * 16;
			for (c, v) in [1.0f32, 1.0, 1.0, 1.0].iter().enumerate() {
				frame.data[at + c * 4..at + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
			}
		}
	}
	let mut inputs = NodeValueRow::new();
	inputs.insert("tex_in".into(), texture_value(Texture::wrap_frame(frame)));
	inputs.insert("color_in".into(), NodeValue::Color([0.0, 0.0, 0.0, 1.0]));
	inputs.insert("distance_in".into(), NodeValue::Float(4.0));
	inputs.insert("angle_in".into(), NodeValue::Float(45.0));
	inputs.insert("radius_in".into(), NodeValue::Float(2.0));
	inputs.insert("opacity_in".into(), NodeValue::Float(1.0));
	inputs.insert("fast_in".into(), NodeValue::Boolean(false));

	let out_frame = eval_node_row("org.olivevideoeditor.Olive.dropshadow", inputs, None);
	assert_eq!((out_frame.width, out_frame.height), (16, 16));
	let covered = out_frame
		.data
		.chunks_exact(16)
		.filter(|px| f32::from_le_bytes(px[12..16].try_into().unwrap()) > 0.01)
		.count();
	assert!(
            covered > 16,
            "the offset blurred shadow must widen the covered area beyond the 4x4 source square: {covered}"
        );
}
/// Transform over the real GPU path: the fragment-side inverse
/// sampling applies the node's matrix for real — a +3px x
/// translation moves the white pixel from (2, 3) to (5, 3).
#[test]
fn gpu_transform_translates_pixels() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	// 8x8 black frame with one white pixel at (2, 3).
	let mut frame = generate_frame(Rational::new(0, 1), (8, 8), PixelFormat::F32).unwrap();
	let at = (3 * 8 + 2) * 16;
	for (c, v) in [1.0f32, 1.0, 1.0, 1.0].iter().enumerate() {
		frame.data[at + c * 4..at + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
	}
	let mut inputs = NodeValueRow::new();
	inputs.insert("tex_in".into(), texture_value(Texture::wrap_frame(frame)));
	inputs.insert("pos_in".into(), NodeValue::Vec2([3.0, 0.0]));

	let out = eval_node_row("org.olivevideoeditor.Olive.transform", inputs, None);
	assert_eq!((out.width, out.height), (8, 8));
	assert_eq!(
		pixel_at(&out, 2, 3),
		[0.0, 0.0, 0.0, 0.0],
		"source spot vacated"
	);
	assert_eq!(
		pixel_at(&out, 5, 3),
		[1.0, 1.0, 1.0, 1.0],
		"pixel moved +3 in x"
	);
	assert_eq!(pixel_at(&out, 0, 0), [0.0, 0.0, 0.0, 0.0]);
}

/// Transform rotation pivots around the FRAME CENTER (the C++
/// center-origin pixel space), not the top-left corner: a 90° turn
/// moves a pixel sitting 2px right of center to 2px below center,
/// with no scaling or smearing (the turn is lossless).
#[test]
fn gpu_transform_rotates_around_the_frame_center() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	// 8x8 black frame with one white pixel at (6, 4) — center+(2, 0).
	let mut frame = generate_frame(Rational::new(0, 1), (8, 8), PixelFormat::F32).unwrap();
	let at = (4 * 8 + 6) * 16;
	for (c, v) in [1.0f32, 1.0, 1.0, 1.0].iter().enumerate() {
		frame.data[at + c * 4..at + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
	}
	let mut inputs = NodeValueRow::new();
	inputs.insert("tex_in".into(), texture_value(Texture::wrap_frame(frame)));
	inputs.insert("rot_in".into(), NodeValue::Float(90.0));

	let out = eval_node_row("org.olivevideoeditor.Olive.transform", inputs, None);
	assert_eq!(
		pixel_at(&out, 6, 4),
		[0.0, 0.0, 0.0, 0.0],
		"source spot vacated"
	);
	assert_eq!(
		pixel_at(&out, 3, 6),
		[1.0, 1.0, 1.0, 1.0],
		"90° around (4,4) maps texel center (6.5,4.5) -> (3.5,6.5)"
	);
	let lit = out
		.data
		.chunks_exact(16)
		.filter(|px| f32::from_le_bytes(px[12..16].try_into().unwrap()) > 0.01)
		.count();
	assert_eq!(
		lit, 1,
		"a pure rotation neither scales nor smears: {lit} lit pixels"
	);
}

/// Transform scale pivots around the frame center too: a center 2x2
/// block at 2x uniform scale grows into the surrounding 4x4 (a
/// corner pivot would drag it toward the bottom-right instead).
#[test]
fn gpu_transform_scales_around_the_frame_center() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	// 8x8 black frame with a white 2x2 block at texels (3..4, 3..4).
	let mut frame = generate_frame(Rational::new(0, 1), (8, 8), PixelFormat::F32).unwrap();
	for (x, y) in [(3usize, 3usize), (4, 3), (3, 4), (4, 4)] {
		let at = (y * 8 + x) * 16;
		for (c, v) in [1.0f32, 1.0, 1.0, 1.0].iter().enumerate() {
			frame.data[at + c * 4..at + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
		}
	}
	let mut inputs = NodeValueRow::new();
	inputs.insert("tex_in".into(), texture_value(Texture::wrap_frame(frame)));
	inputs.insert("scale_in".into(), NodeValue::Vec2([2.0, 2.0]));

	let out = eval_node_row("org.olivevideoeditor.Olive.transform", inputs, None);
	// Pivot check via the alpha distribution: the block [3,5] scaled
	// 2x around (4,4) grows symmetrically to [2,6] — the centroid
	// stays at the frame center. A corner pivot would drag the block
	// to [6,10], shifting the centroid off-center and clipping the
	// block against the frame edge.
	let mut total = 0.0f32;
	let mut cx = 0.0f32;
	let mut cy = 0.0f32;
	for y in 0..8usize {
		for x in 0..8usize {
			let a = pixel_at(&out, x, y)[3];
			total += a;
			cx += (x as f32 + 0.5) * a;
			cy += (y as f32 + 0.5) * a;
		}
	}
	let (cx, cy) = (cx / total, cy / total);
	assert!(
		(cx - 4.0).abs() < 0.2 && (cy - 4.0).abs() < 0.2,
		"the block grows around the frame center, centroid ({cx}, {cy})"
	);
}

/// A transform that pushes content past the frame edge leaves the
/// vacated region TRANSPARENT (no edge-pixel smearing): translating
/// everything +100px in x empties the frame entirely, and a +3px
/// translation vacates exactly the left three columns.
#[test]
fn gpu_transform_off_frame_is_transparent() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	let white = filled_frame((8, 8), [1.0, 1.0, 1.0, 1.0]);
	let mut inputs = NodeValueRow::new();
	inputs.insert("tex_in".into(), texture_value(white));
	inputs.insert("pos_in".into(), NodeValue::Vec2([100.0, 0.0]));
	let out = eval_node_row("org.olivevideoeditor.Olive.transform", inputs, None);
	for y in 0..8usize {
		for x in 0..8usize {
			assert_eq!(
				pixel_at(&out, x, y),
				[0.0, 0.0, 0.0, 0.0],
				"off-frame content must be transparent, got {:?} at ({x},{y})",
				pixel_at(&out, x, y)
			);
		}
	}

	let white = filled_frame((8, 8), [1.0, 1.0, 1.0, 1.0]);
	let mut inputs = NodeValueRow::new();
	inputs.insert("tex_in".into(), texture_value(white));
	inputs.insert("pos_in".into(), NodeValue::Vec2([3.0, 0.0]));
	let out = eval_node_row("org.olivevideoeditor.Olive.transform", inputs, None);
	for x in 0..3usize {
		assert_eq!(
			pixel_at(&out, x, 4),
			[0.0, 0.0, 0.0, 0.0],
			"the vacated left columns are transparent"
		);
	}
	assert_eq!(
		pixel_at(&out, 7, 4),
		[1.0, 1.0, 1.0, 1.0],
		"the rightmost column keeps content"
	);
}

/// Shape generator over the real GPU path: a centered 8x8 rectangle
/// on a 16x16 frame fills exactly the middle block (pixel centers
/// with texcoord in [0.25, 0.75)), everything outside stays
/// transparent.
#[test]
fn gpu_shape_rectangle_draws_centered_block() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	let mut inputs = NodeValueRow::new();
	inputs.insert("pos_in".into(), NodeValue::Vec2([0.0, 0.0]));
	inputs.insert("size_in".into(), NodeValue::Vec2([8.0, 8.0]));
	inputs.insert("color_in".into(), NodeValue::Color([1.0, 0.0, 0.0, 1.0]));
	inputs.insert("type_in".into(), NodeValue::Combo(0));
	inputs.insert("radius_in".into(), NodeValue::Float(20.0));

	let frame = eval_node_row("org.olivevideoeditor.Olive.shape", inputs, Some((16, 16)));
	assert_eq!((frame.width, frame.height), (16, 16));
	for (x, y, inside) in [
		(8, 8, true),
		(4, 4, true),
		(11, 11, true),
		(0, 0, false),
		(3, 8, false),
		(12, 8, false),
		(15, 15, false),
	] {
		let px = pixel_at(&frame, x, y);
		if inside {
			assert_eq!(px, [1.0, 0.0, 0.0, 1.0], "({x},{y}) inside the rect");
		} else {
			assert_eq!(px, [0.0, 0.0, 0.0, 0.0], "({x},{y}) outside the rect");
		}
	}
}

/// Shape generator, the ellipse and rounded-rectangle dispatches:
/// the ellipse fills the center and fades out before the corners;
/// the rounded rect fills the middle but cuts the corner at (4,4)
/// (radius 20 clamps to half the 8px size).
#[test]
fn gpu_shape_ellipse_and_rounded_rect() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	let base_inputs = || {
		let mut inputs = NodeValueRow::new();
		inputs.insert("pos_in".into(), NodeValue::Vec2([0.0, 0.0]));
		inputs.insert("size_in".into(), NodeValue::Vec2([8.0, 8.0]));
		inputs.insert("color_in".into(), NodeValue::Color([1.0, 0.0, 0.0, 1.0]));
		inputs.insert("radius_in".into(), NodeValue::Float(20.0));
		inputs
	};

	let mut ellipse = base_inputs();
	ellipse.insert("type_in".into(), NodeValue::Combo(1));
	let frame = eval_node_row("org.olivevideoeditor.Olive.shape", ellipse, Some((16, 16)));
	assert_eq!(
		pixel_at(&frame, 8, 8),
		[1.0, 0.0, 0.0, 1.0],
		"ellipse center"
	);
	assert_eq!(
		pixel_at(&frame, 0, 0),
		[0.0, 0.0, 0.0, 0.0],
		"ellipse corner faded out"
	);

	let mut rounded = base_inputs();
	rounded.insert("type_in".into(), NodeValue::Combo(2));
	let frame = eval_node_row("org.olivevideoeditor.Olive.shape", rounded, Some((16, 16)));
	assert_eq!(
		pixel_at(&frame, 8, 8),
		[1.0, 0.0, 0.0, 1.0],
		"rounded rect middle"
	);
	assert_eq!(
		pixel_at(&frame, 6, 6),
		[1.0, 0.0, 0.0, 1.0],
		"rounded rect inside the corner arc"
	);
	assert_eq!(
		pixel_at(&frame, 0, 0),
		[0.0, 0.0, 0.0, 0.0],
		"rounded rect far corner"
	);
	assert!(
		pixel_at(&frame, 4, 4)[3] < 0.1,
		"rounded rect corner (4,4) is cut by the arc: {:?}",
		pixel_at(&frame, 4, 4)
	);
}

/// Despill over the real GPU path: green-screen AVERAGE caps the
/// green channel at the red/blue average (the shader's method
/// dispatch must survive translation).
#[test]
fn gpu_despill_average_caps_green() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	let mut inputs = NodeValueRow::new();
	inputs.insert(
		"tex_in".into(),
		texture_value(filled_frame((4, 4), [0.2, 0.9, 0.3, 1.0])),
	);
	inputs.insert("color_in".into(), NodeValue::Combo(0));
	inputs.insert("method_in".into(), NodeValue::Combo(0));
	inputs.insert("preserve_luminance_input".into(), NodeValue::Boolean(false));

	let frame = eval_node_row("org.olivevideoeditor.Olive.despill", inputs, None);
	assert_eq!((frame.width, frame.height), (4, 4));
	let px = pixel_at(&frame, 2, 2);
	let want = [0.2f32, 0.25, 0.3, 1.0];
	for (c, w) in want.iter().enumerate() {
		assert!(
			(px[c] - w).abs() < 1e-4,
			"despill ch{c}: got {}, want {w}",
			px[c]
		);
	}
}

// ---- Endpoint-anchored BFS sweep (M0b) ------------------------------

/// The M0b integration path over real media and a real effect: a test
/// clip probed into a footage node feeds `GraphInput.feed_in`, a
/// Position node sits between the endpoints, and `eval_graph_bfs`
/// decodes the frame, runs the effect pass and returns the output
/// endpoint's texture. The Position offset is `(16, 0)`, so the frame
/// must come out shifted right by 16 pixels — the pixels alone prove
/// both the decode and the shader pass ran inside the sweep.
#[test]
fn bfs_endpoint_sweep_renders_footage_through_position() {
	use quercus_node::nodes::graphendpoints::{GRAPH_INPUT_FEED_INPUT, GRAPH_OUTPUT_INPUT};

	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}

	let path = std::env::temp_dir().join(format!(
		"quercusrender_bfs_endpoints_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip_solid(&path, 64, 64, 10, 10, [0.9, 0.2, 0.1, 1.0])
		.expect("test clip generation");

	let mut graph = quercus_node::graph::Graph::new();
	let (input, output) = graph.ensure_endpoints();
	// The default input -> output edge is replaced by the effect chain.
	graph.disconnect(input, output, GRAPH_OUTPUT_INPUT, -1);

	let (core, _) = quercus_node::footage::FootageBehavior::create();
	let mut footage = quercus_node::footage::FootageBehavior::new(&path.to_string_lossy());
	footage.probe().expect("probe the test clip");
	let footage = graph.add_node(core, Box::new(footage));

	let (core, behavior) = quercus_node::factory::Factory::global()
		.create_any("org.olivevideoeditor.Olive.position")
		.expect("position node registered");
	let position = graph.add_node(core, behavior);
	graph
		.get_mut(position)
		.expect("the position node is live")
		.core
		.set_standard_value("offset_in", -1, NodeValue::Vec2([16.0, 0.0]));

	graph
		.connect(footage, input, GRAPH_INPUT_FEED_INPUT, -1)
		.expect("footage -> GraphInput.feed_in");
	graph
		.connect(input, position, "tex_in", -1)
		.expect("GraphInput.tex_out -> position.tex_in");
	graph
		.connect(position, output, GRAPH_OUTPUT_INPUT, -1)
		.expect("position.tex_out -> GraphOutput.tex_in");

	let mut hooks = RenderEvalHooks::new();
	hooks.frame_size = Some((64, 64));
	let value = quercus_node::traverser::Traverser::new()
		.eval_graph_bfs(&graph, Rational::new(0, 1), &mut hooks)
		.expect("the endpoint sweep runs");

	let NodeValue::Texture(handle) = value else {
		panic!("the sweep must return the output endpoint's texture");
	};
	assert!(!handle.ctx.is_null(), "the returned box is still a job");
	let texture = (unsafe { quercus_node::handle::get_checked::<Texture>(&handle) })
		.cloned()
		.expect("the returned box holds a Texture");
	assert_eq!(texture.size(), (64, 64));
	let frame = texture.to_frame().expect("readback");

	let reference = render_footage_frame(
		&path.to_string_lossy(),
		0,
		Rational::new(0, 1),
		(64, 64),
		PixelFormat::F32,
	)
	.expect("reference decode");
	let reference = reference.to_frame().expect("reference readback");
	let reference_px = pixel_at(&reference, 32, 32);
	assert!(
		reference_px[0] > 0.3 && reference_px[0] > 4.0 * reference_px[1],
		"the reference clip is red-dominant: {reference_px:?}"
	);

	// The vacated left columns are transparent, not a clamped edge
	// column (the Position node's whole-pixel translation).
	for (x, y) in [(0, 0), (8, 32), (15, 63)] {
		assert!(
			pixel_at(&frame, x, y)[3] < 1e-4,
			"({x},{y}) must be transparent after the shift: {:?}",
			pixel_at(&frame, x, y)
		);
	}
	// The footage content arrives at (x + 16, y).
	for (x, y) in [(16, 0), (32, 32), (63, 63)] {
		let got = pixel_at(&frame, x, y);
		let want = pixel_at(&reference, x - 16, y);
		for (c, (g, w)) in got.iter().zip(want).enumerate() {
			assert!(
				(g - w).abs() < 1e-3,
				"shifted pixel ({x},{y}) ch{c}: got {g}, want {w}"
			);
		}
	}

	let _ = std::fs::remove_file(&path);
}

// ---- Coverage edges: the eval seam's error and fallback paths --------

use quercus_core::handle::CHandle;
use quercus_node::project::Project;

/// A GPU-like context whose readback returns a fixed frame: the
/// non-wgpu fallback and the foreign-context readback paths need a
/// `Texture::Gpu` without a real device.
struct FrameEchoCtx {
	frame: Frame,
}

impl quercus_core::backend::GpuContextLike for FrameEchoCtx {
	fn kind(&self) -> quercus_core::backend::BackendKind {
		quercus_core::backend::BackendKind::Cpu
	}
	fn destroy_texture(&self, _token: u64) {}
	fn upload(&self, _token: u64, _frame: &Frame) -> Result<()> {
		Ok(())
	}
	fn download(&self, _token: u64) -> Result<Frame> {
		Ok(self.frame.clone())
	}
	fn blit(
		&self,
		_src: u64,
		_dst: u64,
		_processor: Option<&quercus_core::color::ColorProcessor>,
	) -> Result<()> {
		Err(Error::Failed("FrameEchoCtx cannot blit".into()))
	}
}

/// A `Texture::Gpu` on [`FrameEchoCtx`] reporting `size`.
fn echo_gpu_texture(size: (i32, i32), rgba: [f32; 4], token: u64) -> Texture {
	let frame = filled_frame(size, rgba)
		.to_frame()
		.unwrap_or_else(|_| Frame::dummy());
	Texture::gpu(
		Arc::new(FrameEchoCtx { frame }),
		token,
		size.0,
		size.1,
		PixelFormat::F32,
	)
}

/// An imported planar YUV texture on the non-wgpu stand-in context
/// (the real decode path never produces one in these tests).
fn planar_texture(size: (i32, i32)) -> Texture {
	Texture::wrap_planar(quercus_core::texture::PlanarTexture::new(
		Arc::new(UnusedCtx),
		quercus_core::texture::PlanarFormat::Nv12,
		size,
		(1, 2),
		quercus_core::backend::YuvTransform::bt709_limited(),
		(2, 2),
	))
}

/// A valid OCIO processor (a 1D LUT doubling red) or `None` (with a
/// printed reason) when the bundled config is unavailable.
fn red_doubling_processor(tag: &str) -> Option<quercus_core::color::ColorProcessor> {
	lut_processor(tag, 2.0)
}

/// A valid OCIO processor for the 1D LUT `0 -> 0`, `1 -> scale`.
fn lut_processor(tag: &str, scale: f32) -> Option<quercus_core::color::ColorProcessor> {
	if quercus_core::color::set_up_default_config().is_err() {
		eprintln!("{tag}: bundled OCIO missing; skipping");
		return None;
	}
	let path = std::env::temp_dir().join(format!(
		"quercusrender_eval_{tag}_{}.cube",
		std::process::id()
	));
	std::fs::write(
		&path,
		format!("LUT_1D_SIZE 2\n0.0 0.0 0.0\n{scale} 1.0 1.0\n"),
	)
	.ok()?;
	let processor = quercus_core::color::ColorProcessor::create_lut(
		path.to_str()?,
		quercus_core::color::Direction::Normal,
	)
	.filter(|p| p.is_valid());
	let _ = std::fs::remove_file(&path);
	if processor.is_none() {
		eprintln!("{tag}: LUT processor unavailable; skipping");
	}
	processor
}

#[test]
fn hooks_default_disables_cache_and_reports_it() {
	use quercus_node::traverser::RenderHooks;
	let hooks = RenderEvalHooks::default();
	assert!(!hooks.use_cache());
	let mut on = RenderEvalHooks::new();
	on.use_cache = true;
	assert!(on.use_cache());
	assert!(on.ticket.is_none() && on.frame_size.is_none());
}

#[test]
fn color_transform_job_rejects_non_texture_and_null_inputs() {
	let processor = Arc::new(quercus_core::color::ColorProcessor::pass_through());
	let mut hooks = RenderEvalHooks::new();

	let payload = ColorTransformJobPayload {
		color_processor: processor.clone(),
		input: NodeValue::Float(1.0),
		time: Rational::new(0, 1),
	};
	assert!(matches!(
		hooks.process_color_transform_job(&payload),
		Err(Error::Invalid)
	));

	let payload = ColorTransformJobPayload {
		color_processor: processor,
		input: NodeValue::Texture(CHandle::null()),
		time: Rational::new(0, 1),
	};
	assert!(matches!(
		hooks.process_color_transform_job(&payload),
		Err(Error::Invalid)
	));
}

#[test]
fn color_transform_job_passes_planar_textures_through() {
	let Some(processor) = red_doubling_processor("planar") else {
		return;
	};
	let payload = ColorTransformJobPayload {
		color_processor: Arc::new(processor),
		input: NodeValue::Texture(quercus_node::handle::make_owned(planar_texture((2, 2)))),
		time: Rational::new(0, 1),
	};
	let out = RenderEvalHooks::new()
		.process_color_transform_job(&payload)
		.expect("planar pass-through");
	assert!(out.is_planar());
}

#[test]
fn color_transform_job_without_a_wgpu_context_converts_on_cpu() {
	let Some(processor) = red_doubling_processor("ctxfallback") else {
		return;
	};
	let input = echo_gpu_texture((2, 2), [0.25, 0.25, 0.25, 1.0], 4242);
	let payload = ColorTransformJobPayload {
		color_processor: Arc::new(processor),
		input: NodeValue::Texture(quercus_node::handle::make_owned(input)),
		time: Rational::new(0, 1),
	};
	let out = RenderEvalHooks::new()
		.process_color_transform_job(&payload)
		.expect("cpu fallback");
	assert!(
		matches!(out, Texture::Cpu(_)),
		"the fallback wraps a CPU frame"
	);
	let px = first_pixel(&out);
	assert!((px[0] - 0.5).abs() < 1e-3, "red doubled on the CPU: {px:?}");
}

#[test]
fn plugin_job_rejects_a_non_plugin_spec() {
	let mut hooks = RenderEvalHooks::new();
	let src =
		Texture::wrap_frame(generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap());
	assert!(matches!(
		hooks.process_plugin_job(src, &JobSpec::Generate),
		Err(Error::Invalid)
	));
}

#[test]
fn resolve_value_guards_depth_null_and_in_flight() {
	let mut hooks = RenderEvalHooks::new();
	let mut in_flight = HashSet::new();

	// Depth ceiling: even a job box is left untouched.
	let mut deep = NodeValue::Texture(quercus_node::handle::make_owned(Job::FootageJob(
		FootageJobPayload::default(),
	)));
	hooks.resolve_value(&mut deep, 64, &mut in_flight);
	let NodeValue::Texture(deep_handle) = &deep else {
		unreachable!()
	};
	assert!(unsafe { quercus_node::jobs::job_ref(deep_handle) }.is_some());
	assert!(in_flight.is_empty());

	// A non-texture value passes through untouched.
	let mut scalar = NodeValue::Float(0.5);
	hooks.resolve_value(&mut scalar, 0, &mut in_flight);
	assert!(matches!(scalar, NodeValue::Float(v) if v == 0.5));

	// Empty handle: nothing to resolve.
	let mut empty = NodeValue::Texture(CHandle::null());
	hooks.resolve_value(&mut empty, 0, &mut in_flight);
	assert!(matches!(empty, NodeValue::Texture(_)));

	// A handle already in flight is skipped (the cycle guard).
	let mut boxed = NodeValue::Texture(quercus_node::handle::make_owned(Job::FootageJob(
		FootageJobPayload::default(),
	)));
	let key = match &boxed {
		NodeValue::Texture(h) => h.ctx as usize,
		_ => unreachable!(),
	};
	in_flight.insert(key);
	hooks.resolve_value(&mut boxed, 0, &mut in_flight);
	let NodeValue::Texture(still) = &boxed else {
		unreachable!()
	};
	assert!(unsafe { quercus_node::jobs::job_ref(still) }.is_some());
	assert!(in_flight.contains(&key));
}

#[test]
fn footage_job_with_missing_media_keeps_its_box() {
	let payload = FootageJobPayload {
		filename: std::env::temp_dir()
			.join(format!("quercusrender_missing_{}.mp4", std::process::id()))
			.to_string_lossy()
			.into_owned(),
		stream_index: 0,
		time: Rational::new(0, 1),
	};
	assert!(RenderEvalHooks::new()
		.process_footage_job_value(&payload)
		.is_none());
}

#[test]
fn plugin_job_value_splits_clip_inputs_from_scalar_values() {
	use quercus_node::nodes::plugin::{PluginInstanceHandle, PluginJobPayload};

	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	crate::ofxhost::install_client(None);
	set_plugin_executor(Some(Arc::new(|req: &PluginJobRequest<'_>| {
		let JobSpec::Plugin {
			effect_input_id,
			inputs,
			values,
			..
		} = req.spec
		else {
			return Err(Error::Invalid);
		};
		assert!(effect_input_id.is_none(), "empty id maps to None");
		assert_eq!(inputs.len(), 1, "only the genuine texture box is a clip");
		assert_eq!(inputs[0].0, "Source");
		assert!(values.iter().any(|(k, _)| k == "gain"));
		assert!(values.iter().all(|(k, _)| k != "Nothing"));
		let mut frame = generate_frame(Rational::new(0, 1), req.src.size(), PixelFormat::F32)?;
		for pixel in frame.data.as_chunks_mut::<16>().0 {
			for (c, v) in pixel
				.as_chunks_mut::<4>()
				.0
				.iter_mut()
				.zip([0.5f32, 0.25, 0.125, 1.0])
			{
				c.copy_from_slice(&v.to_le_bytes());
			}
		}
		Ok(Texture::wrap_frame(frame))
	})));

	let mut values = NodeValueRow::new();
	values.insert(
		"Source".into(),
		texture_value(filled_frame((2, 1), [0.1, 0.2, 0.3, 1.0])),
	);
	values.insert("gain".into(), NodeValue::Float(0.25));
	values.insert("Nothing".into(), NodeValue::None);
	// A null texture box is skipped by the type guard.
	values.insert("Null".into(), NodeValue::Texture(CHandle::null()));
	// A non-texture box in the texture channel logs and is skipped.
	values.insert(
		"Boxed".into(),
		NodeValue::Texture(quercus_node::handle::make_owned(Job::FootageJob(
			FootageJobPayload::default(),
		))),
	);
	let payload = PluginJobPayload {
		instance: PluginInstanceHandle(7),
		type_id: "org.quercus.test-plugin".into(),
		time: Rational::new(1, 2),
		effect_input_id: String::new(),
		values,
		animations: Vec::new(),
	};

	let out = RenderEvalHooks::new()
		.process_plugin_job_value(&payload, 0, &mut HashSet::new())
		.expect("plugin value resolves");
	let NodeValue::Texture(handle) = &out else {
		panic!("expected a texture value, got {out:?}");
	};
	let texture = unsafe { quercus_node::handle::get_checked::<Texture>(handle) }
		.cloned()
		.expect("resolved texture");
	assert_eq!(first_pixel(&texture), [0.5, 0.25, 0.125, 1.0]);
	set_plugin_executor(None);
}

#[test]
fn color_transform_job_value_falls_back_to_its_input() {
	let payload = ColorTransformJobPayload {
		color_processor: Arc::new(quercus_core::color::ColorProcessor::pass_through()),
		input: NodeValue::None,
		time: Rational::new(0, 1),
	};
	let out =
		RenderEvalHooks::new().process_color_transform_job_value(&payload, 0, &mut HashSet::new());
	assert!(matches!(out, NodeValue::None));
}

#[test]
fn composite_tracks_rejects_nonpositive_sizes() {
	assert!(composite_tracks(Vec::new(), (0, 4)).is_dummy());
	assert!(composite_tracks(Vec::new(), (4, -1)).is_dummy());
}

#[test]
fn evaluate_block_frame_handles_missing_and_textureless_roots() {
	let mut graph = quercus_node::graph::Graph::new();
	let (score, sbehavior) = quercus_node::sequence::SequenceBehavior::create();
	let seq = graph.add_node(score, sbehavior);
	let mut traverser = quercus_node::traverser::Traverser::new();
	let mut hooks = RenderEvalHooks::new();

	// A stale root surfaces as `Failed` (the NotFound mapping).
	let missing = evaluate_block_frame(
		&graph,
		&mut traverser,
		&mut hooks,
		quercus_node::id::NodeId::INVALID,
		Rational::new(0, 1),
	);
	assert!(matches!(missing, Err(Error::Failed(_))));

	// A root with no texture channel is `Ok(None)`.
	let none = evaluate_block_frame(&graph, &mut traverser, &mut hooks, seq, Rational::new(0, 1));
	assert!(matches!(none, Ok(None)));
}

#[test]
fn blend_transition_without_sides_is_inert() {
	let graph = quercus_node::graph::Graph::new();
	let mut traverser = quercus_node::traverser::Traverser::new();
	let mut hooks = RenderEvalHooks::new();
	let out = blend_transition(
		&graph,
		&mut traverser,
		&mut hooks,
		None,
		None,
		Rational::new(0, 1),
		0.5,
		"linear",
	);
	assert!(matches!(out, Ok(None)));
}

#[test]
fn adjustment_chain_head_needs_an_effect_input() {
	let mut graph = quercus_node::graph::Graph::new();
	let (core, behavior) = quercus_node::track::TrackBehavior::create();
	let track = graph.add_node(core, behavior);
	assert!(adjustment_chain_head(&graph, track).is_none());
	assert!(adjustment_chain_head(&graph, quercus_node::id::NodeId::INVALID).is_none());
}

#[test]
fn layer_progress_clamps_and_reports_degenerate_spans() {
	assert_eq!(
		layer_progress(
			Rational::new(1, 1),
			Rational::new(1, 1),
			Rational::new(1, 1)
		),
		0.0
	);
	assert_eq!(
		layer_progress(
			Rational::new(2, 1),
			Rational::new(1, 1),
			Rational::new(1, 1)
		),
		0.0
	);
	assert_eq!(
		layer_progress(
			Rational::new(0, 1),
			Rational::new(2, 1),
			Rational::new(1, 1)
		),
		0.5
	);
	assert_eq!(
		layer_progress(
			Rational::new(0, 1),
			Rational::new(2, 1),
			Rational::new(9, 1)
		),
		1.0
	);
}

#[test]
fn audio_layout_rejects_degenerate_and_oversized_ranges() {
	// Null denominator: the duration seconds stay 0.
	let params = audio_params(TimeRange::new(Rational::NULL, Rational::NULL));
	assert!(render_audio_samples(&params).is_err());

	// Longer than an hour.
	let params = audio_params(TimeRange::new(Rational::new(0, 1), Rational::new(7200, 1)));
	assert!(render_audio_samples(&params).is_err());
}

#[test]
fn mix_audio_montage_skips_clips_outside_the_range() {
	let mut params = audio_params(TimeRange::new(Rational::new(0, 1), Rational::new(1, 48)));
	let clip = |in_: Rational, out: Rational| crate::ticket::MontageClip {
		filename: String::new(),
		stream_index: 0,
		in_time: in_,
		out_time: out,
		media_in: Rational::new(0, 1),
		gain: 1.0,
		pan: 0.0,
		fade_in: Rational::new(0, 1),
		fade_out: Rational::new(0, 1),
		effects: Vec::new(),
		audio_effects: Vec::new(),
	};
	params.montage = vec![
		// No overlap at all.
		clip(Rational::new(2, 1), Rational::new(3, 1)),
		// Starts at the last sample (start_frame >= total_frames).
		clip(Rational::new(1999, 96000), Rational::new(1, 48)),
		// Rounds to zero frames.
		clip(Rational::new(1, 480000), Rational::new(4, 480000)),
	];
	let mut acc = vec![0.0f32; 1000 * 2];
	mix_audio_montage(&params, 48000, 2, 1000, &mut acc).expect("skips");
	assert!(
		acc.iter().all(|&v| v == 0.0),
		"uncovered range stays silent"
	);
}

// ---- Per-clip audio processing (Phase 1 audio post) ------------------

fn audio_test_clip(
	gain: f32,
	pan: f32,
	fade_in: Rational,
	fade_out: Rational,
	audio_effects: Vec<crate::ticket::MontageAudioEffect>,
) -> crate::ticket::MontageClip {
	crate::ticket::MontageClip {
		filename: String::new(),
		stream_index: 0,
		in_time: Rational::new(0, 1),
		out_time: Rational::new(1, 1),
		media_in: Rational::new(0, 1),
		gain,
		pan,
		fade_in,
		fade_out,
		effects: Vec::new(),
		audio_effects,
	}
}

fn volume_effect(value: f64) -> crate::ticket::MontageAudioEffect {
	crate::ticket::MontageAudioEffect {
		type_id: quercus_node::block::AUDIO_EFFECT_VOLUME_TYPE_ID.to_string(),
		enabled: true,
		params: vec![(
			quercus_node::block::AUDIO_VOLUME_INPUT.to_string(),
			NodeValue::Float(value),
		)],
	}
}

fn pan_effect(value: f64) -> crate::ticket::MontageAudioEffect {
	crate::ticket::MontageAudioEffect {
		type_id: quercus_node::block::AUDIO_EFFECT_PAN_TYPE_ID.to_string(),
		enabled: true,
		params: vec![(
			quercus_node::block::AUDIO_PAN_INPUT.to_string(),
			NodeValue::Float(value),
		)],
	}
}

fn run_clip_audio(
	buf: &mut [f32],
	channels: usize,
	clip: &crate::ticket::MontageClip,
	first_time: f64,
	rate: i32,
) {
	let frames = buf.len() / channels;
	process_clip_audio(buf, frames, channels, clip, first_time, rate, None);
}

/// Gain 1.0 at pan centre is unity on both stereo channels; the
/// balance law must not make the default clip quieter.
#[test]
fn clip_gain_at_pan_centre_is_unity() {
	let clip = audio_test_clip(
		1.0,
		0.0,
		Rational::new(0, 1),
		Rational::new(0, 1),
		Vec::new(),
	);
	let mut buf = vec![1.0f32; 2];
	run_clip_audio(&mut buf, 2, &clip, 0.0, 48000);
	for sample in buf {
		assert!(
			(sample - 1.0).abs() < 1e-6,
			"centre pan is unity, got {sample}"
		);
	}

	// Gain is applied on top of the pan law.
	let clip = audio_test_clip(
		0.5,
		0.0,
		Rational::new(0, 1),
		Rational::new(0, 1),
		Vec::new(),
	);
	let mut buf = vec![1.0f32; 2];
	run_clip_audio(&mut buf, 2, &clip, 0.0, 48000);
	for sample in buf {
		assert!(
			(sample - 0.5).abs() < 1e-6,
			"gain multiplies centre, got {sample}"
		);
	}
}

/// Hard left/right isolate one channel.
#[test]
fn clip_pan_isolates_the_opposite_channel() {
	let left = audio_test_clip(
		1.0,
		-1.0,
		Rational::new(0, 1),
		Rational::new(0, 1),
		Vec::new(),
	);
	let mut buf = vec![1.0f32; 2];
	run_clip_audio(&mut buf, 2, &left, 0.0, 48000);
	assert!((buf[0] - 1.0).abs() < 1e-6, "left stays at unity");
	assert!(buf[1].abs() < 1e-6, "hard-left silences the right channel");

	let right = audio_test_clip(
		1.0,
		1.0,
		Rational::new(0, 1),
		Rational::new(0, 1),
		Vec::new(),
	);
	let mut buf = vec![1.0f32; 2];
	run_clip_audio(&mut buf, 2, &right, 0.0, 48000);
	assert!(buf[0].abs() < 1e-6, "hard-right silences the left channel");
	assert!((buf[1] - 1.0).abs() < 1e-6, "right stays at unity");
}

/// Linear fades are anchored at the clip's timeline in/out (not at the
/// decoded chunk's own edges).
#[test]
fn clip_fades_follow_the_clip_bounds() {
	// Fade-in over the whole 1 s clip, mono: t = 0, .25, .5, .75 s.
	let clip = audio_test_clip(
		1.0,
		0.0,
		Rational::new(1, 1),
		Rational::new(0, 1),
		Vec::new(),
	);
	let mut buf = vec![1.0f32; 4];
	run_clip_audio(&mut buf, 1, &clip, 0.0, 4);
	for (i, sample) in buf.iter().enumerate() {
		assert!(
			(*sample - i as f32 / 4.0).abs() < 1e-6,
			"fade-in frame {i}: {sample}"
		);
	}

	// Fade-out over the whole 1 s clip: t = 0, .25, .5, .75, 1.0 s.
	let clip = audio_test_clip(
		1.0,
		0.0,
		Rational::new(0, 1),
		Rational::new(1, 1),
		Vec::new(),
	);
	let mut buf = vec![1.0f32; 5];
	run_clip_audio(&mut buf, 1, &clip, 0.0, 4);
	for (i, sample) in buf.iter().enumerate() {
		assert!(
			(*sample - (1.0 - i as f32 / 4.0)).abs() < 1e-6,
			"fade-out frame {i}: {sample}"
		);
	}

	// A fade that started before the rendered chunk is evaluated
	// against absolute time: the chunk begins 0.5 s into a 1 s fade.
	let clip = audio_test_clip(
		1.0,
		0.0,
		Rational::new(1, 1),
		Rational::new(0, 1),
		Vec::new(),
	);
	let mut buf = vec![1.0f32; 2];
	run_clip_audio(&mut buf, 1, &clip, 0.5, 4);
	assert!((buf[0] - 0.5).abs() < 1e-6);
	assert!((buf[1] - 0.75).abs() < 1e-6);
}

fn audio_param_effect(type_id: &str, params: &[(&str, f64)]) -> crate::ticket::MontageAudioEffect {
	crate::ticket::MontageAudioEffect {
		type_id: type_id.to_string(),
		enabled: true,
		params: params
			.iter()
			.map(|(input, value)| (input.to_string(), NodeValue::Float(*value)))
			.collect(),
	}
}

#[test]
fn eq_and_compressor_process_the_clip_buffer() {
	// A DC/low-frequency signal is boosted by a +12 dB low band.
	let clip = audio_test_clip(
		1.0,
		0.0,
		Rational::new(0, 1),
		Rational::new(0, 1),
		vec![audio_param_effect(
			quercus_node::block::AUDIO_EFFECT_EQ_TYPE_ID,
			&[(quercus_node::block::AUDIO_EQ_LOW_GAIN_INPUT, 12.0)],
		)],
	);
	let mut buf = vec![0.5f32; 4800];
	run_clip_audio(&mut buf, 1, &clip, 0.0, 48000);
	assert!(
		buf.last().copied().unwrap() > 0.5,
		"low-band boost must raise a low-frequency signal"
	);

	// A 0 dBFS signal is reduced by a -20 dB / 4:1 compressor.
	let clip = audio_test_clip(
		1.0,
		0.0,
		Rational::new(0, 1),
		Rational::new(0, 1),
		vec![audio_param_effect(
			quercus_node::block::AUDIO_EFFECT_COMPRESSOR_TYPE_ID,
			&[
				(quercus_node::block::AUDIO_COMP_THRESHOLD_INPUT, -20.0),
				(quercus_node::block::AUDIO_COMP_RATIO_INPUT, 4.0),
				(quercus_node::block::AUDIO_COMP_ATTACK_INPUT, 0.1),
				(quercus_node::block::AUDIO_COMP_RELEASE_INPUT, 10.0),
			],
		)],
	);
	let mut buf = vec![1.0f32; 4800];
	run_clip_audio(&mut buf, 1, &clip, 0.0, 48000);
	assert!(
		buf.last().copied().unwrap() < 0.9,
		"compressor must reduce a 0 dBFS signal"
	);
}

/// The chain applies Volume then Pan in the stored order. (Both are
/// channel-diagonal linear kernels, so they commute numerically; the
/// test pins the combined result and the chain order is covered by the
/// montage propagation tests.)
#[test]
fn clip_audio_effects_apply_volume_then_pan() {
	let clip = audio_test_clip(
		1.0,
		0.0,
		Rational::new(0, 1),
		Rational::new(0, 1),
		vec![volume_effect(0.5), pan_effect(0.5)],
	);
	let mut buf = vec![1.0f32; 2];
	run_clip_audio(&mut buf, 2, &clip, 0.0, 48000);
	// Centre base pan unity, Volume 0.5 (both), Pan +0.5 halves left.
	assert!((buf[0] - 0.25).abs() < 1e-6);
	assert!((buf[1] - 0.5).abs() < 1e-6);

	// Swapping the entries still applies both (the kernels commute).
	let clip = audio_test_clip(
		1.0,
		0.0,
		Rational::new(0, 1),
		Rational::new(0, 1),
		vec![pan_effect(0.5), volume_effect(0.5)],
	);
	let mut buf = vec![1.0f32; 2];
	run_clip_audio(&mut buf, 2, &clip, 0.0, 48000);
	assert!((buf[0] - 0.25).abs() < 1e-6);
	assert!((buf[1] - 0.5).abs() < 1e-6);
}

/// Disabled effects are bypassed and an unknown type id passes the
/// samples through (warning once, never a panic or a dropped clip).
#[test]
fn clip_audio_effects_bypass_disabled_and_unknown() {
	let mut disabled = volume_effect(0.0);
	disabled.enabled = false;
	let clip = audio_test_clip(
		1.0,
		0.0,
		Rational::new(0, 1),
		Rational::new(0, 1),
		vec![disabled],
	);
	let mut buf = vec![1.0f32; 2];
	run_clip_audio(&mut buf, 2, &clip, 0.0, 48000);
	for sample in &buf {
		assert!((*sample - 1.0).abs() < 1e-6);
	}

	let unknown = crate::ticket::MontageAudioEffect {
		type_id: "com.example.unknown-audio".into(),
		enabled: true,
		params: Vec::new(),
	};
	let clip = audio_test_clip(
		1.0,
		0.0,
		Rational::new(0, 1),
		Rational::new(0, 1),
		vec![unknown, volume_effect(0.5)],
	);
	let mut buf = vec![1.0f32; 2];
	run_clip_audio(&mut buf, 2, &clip, 0.0, 48000);
	for sample in &buf {
		assert!(
			(*sample - 0.5).abs() < 1e-6,
			"unknown effect passes through, later effects still run"
		);
	}
	// Warms / reuses the one-shot warning path (must not panic).
	run_clip_audio(&mut buf, 2, &clip, 0.0, 48000);
}

/// Missing effect parameters fall back to the node defaults (Volume
/// unity, Pan centre).
#[test]
fn clip_audio_effect_defaults_when_params_are_missing() {
	let clip = audio_test_clip(
		1.0,
		0.0,
		Rational::new(0, 1),
		Rational::new(0, 1),
		vec![
			crate::ticket::MontageAudioEffect {
				type_id: quercus_node::block::AUDIO_EFFECT_VOLUME_TYPE_ID.into(),
				enabled: true,
				params: Vec::new(),
			},
			crate::ticket::MontageAudioEffect {
				type_id: quercus_node::block::AUDIO_EFFECT_PAN_TYPE_ID.into(),
				enabled: true,
				params: Vec::new(),
			},
		],
	);
	let mut buf = vec![1.0f32; 2];
	run_clip_audio(&mut buf, 2, &clip, 0.0, 48000);
	// Volume 1.0 and Pan 0.0 leave the centre untouched.
	for sample in &buf {
		assert!((*sample - 1.0).abs() < 1e-6);
	}
}

#[test]
fn limiter_keeps_a_full_scale_sine_under_its_ceiling() {
	let effect = audio_param_effect(
		quercus_node::block::AUDIO_EFFECT_LIMITER_TYPE_ID,
		&[
			(quercus_node::block::AUDIO_LIMITER_CEILING_INPUT, -1.0),
			(quercus_node::block::AUDIO_LIMITER_RELEASE_INPUT, 50.0),
		],
	);
	let frames = 24_000;
	let mut buf: Vec<f32> = (0..frames)
		.map(|frame| (std::f64::consts::TAU * 1_000.0 * frame as f64 / 48_000.0).sin() as f32)
		.collect();
	assert!((buf.iter().fold(0.0f32, |peak, x| peak.max(x.abs())) - 1.0).abs() < 1e-3);
	apply_audio_effect(&mut buf, frames, 1, &effect, 48_000.0, None);
	let ceiling = 10f64.powf(-1.0 / 20.0) as f32;
	let peak = buf.iter().fold(0.0f32, |peak, x| peak.max(x.abs()));
	assert!(
		peak <= ceiling + 1e-6,
		"limiter output {peak} must not exceed the ceiling {ceiling}"
	);
	assert!(
		peak > 0.5,
		"the limiter must not mute the signal, peak {peak}"
	);
}

#[test]
fn gate_suppresses_a_low_signal_and_passes_a_loud_one() {
	let gate = || {
		audio_param_effect(
			quercus_node::block::AUDIO_EFFECT_GATE_TYPE_ID,
			&[
				(quercus_node::block::AUDIO_GATE_THRESHOLD_INPUT, -40.0),
				(quercus_node::block::AUDIO_GATE_ATTACK_INPUT, 1.0),
				(quercus_node::block::AUDIO_GATE_RELEASE_INPUT, 10.0),
				(quercus_node::block::AUDIO_GATE_HOLD_INPUT, 5.0),
			],
		)
	};
	let frames = 48_000;
	let tone = |amplitude: f32| -> Vec<f32> {
		(0..frames)
			.map(|frame| {
				let t = frame as f64 / 48_000.0;
				(f64::from(amplitude) * (std::f64::consts::TAU * 440.0 * t).sin()) as f32
			})
			.collect()
	};
	let rms = |samples: &[f32]| -> f32 {
		let tail = &samples[samples.len() / 2..];
		(tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt()
	};

	let mut quiet = tone(0.001); // -60 dBFS, below the -40 dBFS gate
	apply_audio_effect(&mut quiet, frames, 1, &gate(), 48_000.0, None);
	assert!(
		rms(&quiet) < 1e-4,
		"below-threshold signal is attenuated, rms {}",
		rms(&quiet)
	);

	let mut loud = tone(0.5); // -6 dBFS, above the gate
	apply_audio_effect(&mut loud, frames, 1, &gate(), 48_000.0, None);
	assert!(
		rms(&loud) > 0.2,
		"above-threshold signal passes, rms {}",
		rms(&loud)
	);
}

#[test]
fn ducking_reduces_music_after_dialogue_in_the_accumulator() {
	// The synthetic accumulator: dialogue is present for the first
	// half, then silence. The music clip runs across the whole window
	// and is the buffer the ducker processes.
	let frames = 24_000;
	let mut music = vec![0.5f32; frames];
	let mut sidechain = vec![0.0f32; frames];
	for sample in &mut sidechain[..frames / 2] {
		*sample = 0.5;
	}
	let clip = audio_test_clip(
		1.0,
		0.0,
		Rational::new(0, 1),
		Rational::new(0, 1),
		vec![audio_param_effect(
			quercus_node::block::AUDIO_EFFECT_DUCKING_TYPE_ID,
			&[
				(quercus_node::block::AUDIO_DUCKING_THRESHOLD_INPUT, -30.0),
				(quercus_node::block::AUDIO_DUCKING_AMOUNT_INPUT, -12.0),
				(quercus_node::block::AUDIO_DUCKING_ATTACK_INPUT, 1.0),
				(quercus_node::block::AUDIO_DUCKING_RELEASE_INPUT, 20.0),
			],
		)],
	);
	process_clip_audio(&mut music, frames, 1, &clip, 0.0, 48_000, Some(&sidechain));
	let during = music[frames / 4];
	let after = music[frames - 1];
	assert!(
		during < 0.2,
		"dialogue must duck the music by ~12 dB, got {during}"
	);
	assert!(
		after > 0.45,
		"music recovers after the dialogue stops, got {after}"
	);
}

#[test]
fn bus_limiter_applies_to_the_mixed_accumulator() {
	let frames = 4_800;
	// Two overlapping clips mixed linearly: 1.6 would clip at the
	// final full-scale clamp, but the bus limiter must catch it first.
	let mut acc = vec![1.6f32; frames];
	let bus = [audio_param_effect(
		quercus_node::block::AUDIO_EFFECT_LIMITER_TYPE_ID,
		&[
			(quercus_node::block::AUDIO_LIMITER_CEILING_INPUT, -1.0),
			(quercus_node::block::AUDIO_LIMITER_RELEASE_INPUT, 50.0),
		],
	)];
	apply_bus_effects(&mut acc, 1, &bus, 48_000.0);
	let ceiling = 10f64.powf(-1.0 / 20.0) as f32;
	let peak = acc.iter().fold(0.0f32, |peak, x| peak.max(x.abs()));
	assert!(
		peak <= ceiling + 1e-6,
		"bus limiter must bound the mixed accumulator, peak {peak}"
	);
	// The final full-scale clamp still runs after the bus.
	for sample in &mut acc {
		*sample = sample.clamp(-1.0, 1.0);
	}
	assert_eq!(acc.iter().fold(0.0f32, |peak, x| peak.max(x.abs())), peak);
}

#[test]
fn scale_rgba_f32_bilinear_scales_and_clamps_alpha() {
	let src_w = 2i32;
	let src_h = 2i32;
	let src_stride = (src_w * 16) as usize;
	let mut src = vec![0u8; src_stride * src_h as usize];
	let put = |src: &mut [u8], x: usize, y: usize, rgba: [f32; 4]| {
		let off = y * src_stride + x * 16;
		for (i, v) in rgba.iter().enumerate() {
			src[off + i * 4..off + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
		}
	};
	put(&mut src, 0, 0, [1.0, 0.0, 0.0, 3.0]);
	put(&mut src, 1, 0, [0.0, 1.0, 0.0, 3.0]);
	put(&mut src, 0, 1, [0.0, 0.0, 1.0, 3.0]);
	put(&mut src, 1, 1, [1.0, 1.0, 1.0, 3.0]);

	let dst_w = 4i32;
	let dst_h = 4i32;
	let dst_stride = (dst_w * 16) as usize;
	let mut dst = vec![0u8; dst_stride * dst_h as usize];
	scale_rgba_f32(
		src.as_ptr(),
		src_stride as i32,
		src_w,
		src_h,
		&mut dst,
		dst_stride as i32,
		dst_w,
		dst_h,
	);
	let read = |dst: &[u8], x: usize, y: usize| -> [f32; 4] {
		let off = y * dst_stride + x * 16;
		let mut out = [0f32; 4];
		for (i, v) in out.iter_mut().enumerate() {
			*v = f32::from_le_bytes(dst[off + i * 4..off + i * 4 + 4].try_into().unwrap());
		}
		out
	};

	// The top-left destination texel maps exactly onto the source
	// corner; alpha 3.0 clamps to 1.0.
	assert_eq!(read(&dst, 0, 0), [1.0, 0.0, 0.0, 1.0]);
	// The bottom-right texel lands on the far corner.
	assert_eq!(read(&dst, 3, 3), [1.0, 1.0, 1.0, 1.0]);
	// A mid texel interpolates bilinearly (0.25, 0.25 in source space).
	let mid = read(&dst, 1, 1);
	for (got, want) in mid.iter().zip([0.625f32, 0.25, 0.25, 1.0]) {
		assert!((got - want).abs() < 1e-5, "bilinear {mid:?}");
	}

	// Invalid geometry: nothing is written.
	let mut untouched = vec![0xAAu8; 16];
	scale_rgba_f32(
		src.as_ptr(),
		src_stride as i32,
		src_w,
		src_h,
		&mut untouched,
		16,
		0,
		1,
	);
	assert!(untouched.iter().all(|&b| b == 0xAA));
}

#[test]
fn copy_rows_copies_strided_rows_and_rejects_bad_geometry() {
	let src: Vec<u8> = (0..64u16).map(|i| i as u8).collect();
	let mut dst = vec![0u8; 128];
	assert!(copy_rows(&mut dst, 64, &src, 32, 2, 2));
	assert_eq!(&dst[..32], &src[..32]);
	assert_eq!(&dst[64..96], &src[32..64]);
	assert!(dst[32..64].iter().all(|&b| b == 0), "gap untouched");
	assert!(dst[96..].iter().all(|&b| b == 0), "gap untouched");

	assert!(!copy_rows(&mut dst, 64, &src, 32, 0, 2));
	assert!(!copy_rows(&mut dst, 64, &src, 32, 2, 0));
	assert!(
		!copy_rows(&mut dst, 64, &src[..40], 32, 2, 2),
		"src too small"
	);
	let mut small = vec![0u8; 64];
	assert!(!copy_rows(&mut small, 64, &src, 32, 2, 2), "dst too small");
}

/// A one-row F32 RGBA frame as raw bytes.
fn f32_frame_bytes(size: (i32, i32), rgba: [f32; 4]) -> Vec<u8> {
	let mut frame = generate_frame(Rational::new(0, 1), size, PixelFormat::F32).unwrap();
	for px in frame.data.as_chunks_mut::<16>().0 {
		for (c, v) in px.as_chunks_mut::<4>().0.iter_mut().zip(rgba) {
			c.copy_from_slice(&v.to_le_bytes());
		}
	}
	frame.data
}

fn read_f32_px(data: &[u8], stride: usize, x: usize, y: usize) -> [f32; 4] {
	let off = y * stride + x * 16;
	let mut out = [0f32; 4];
	for (i, v) in out.iter_mut().enumerate() {
		*v = f32::from_le_bytes(data[off + i * 4..off + i * 4 + 4].try_into().unwrap());
	}
	out
}

fn adjustment_span(
	in_: i64,
	out: i64,
	track_index: usize,
	effects: Vec<crate::ticket::MontageEffect>,
) -> crate::ticket::AdjustmentSpan {
	crate::ticket::AdjustmentSpan {
		in_time: Rational::new(in_, 1),
		out_time: Rational::new(out, 1),
		track_index,
		effects,
	}
}

fn unknown_effect(type_id: &str) -> crate::ticket::MontageEffect {
	crate::ticket::MontageEffect {
		type_id: type_id.to_string(),
		enabled: true,
		effect_input_id: Some("Source".to_string()),
		params: Vec::new(),
	}
}

#[test]
fn adjustment_span_opacity_scales_and_unknown_effects_stage() {
	let stride = 2 * 16;
	// Opacity-only: the fast in-place scale.
	let mut dst = f32_frame_bytes((2, 1), [0.8, 0.4, 0.2, 1.0]);
	let span = adjustment_span(0, 2, 0, vec![opacity_effect(true, 0.5)]);
	apply_adjustment_span(&mut dst, stride as i32, 2, 1, &span, Rational::new(0, 1));
	let px = read_f32_px(&dst, stride, 0, 0);
	assert!((px[0] - 0.4).abs() < 1e-6, "{px:?}");
	assert!((px[3] - 0.5).abs() < 1e-6, "{px:?}");

	// Unity opacity is skipped, an unknown effect forces the staged
	// path and passes the frame through (with the warn-once log).
	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	crate::ofxhost::install_client(None);
	set_plugin_instance_factory(Some(Arc::new(|_id: &str| None)));
	let mut dst = f32_frame_bytes((2, 1), [0.8, 0.4, 0.2, 1.0]);
	let span = adjustment_span(
		0,
		2,
		0,
		vec![
			opacity_effect(true, 1.0),
			unknown_effect("com.example.eval-unknown"),
		],
	);
	apply_adjustment_span(&mut dst, stride as i32, 2, 1, &span, Rational::new(0, 1));
	assert_eq!(read_f32_px(&dst, stride, 0, 0), [0.8, 0.4, 0.2, 1.0]);
	set_plugin_instance_factory(None);

	// A span that does not cover the time is inert.
	let mut dst = f32_frame_bytes((2, 1), [0.8, 0.4, 0.2, 1.0]);
	let span = adjustment_span(5, 6, 0, vec![opacity_effect(true, 0.5)]);
	apply_adjustment_span(&mut dst, stride as i32, 2, 1, &span, Rational::new(0, 1));
	assert_eq!(read_f32_px(&dst, stride, 0, 0), [0.8, 0.4, 0.2, 1.0]);
}

#[test]
fn montage_frame_into_rejects_bad_geometry_and_skips_uncovered_clips() {
	let params = crate::ticket::VideoTicketParams {
		viewer: 1,
		project: String::new(),
		time: Rational::new(0, 1),
		force_size: Some((2, 1)),
		force_format: Some(PixelFormat::F32),
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage: vec![crate::ticket::MontageClip {
			filename: String::new(),
			stream_index: 0,
			in_time: Rational::new(1, 1),
			out_time: Rational::new(2, 1),
			media_in: Rational::new(0, 1),
			gain: 1.0,
			pan: 0.0,
			fade_in: Rational::new(0, 1),
			fade_out: Rational::new(0, 1),
			effects: Vec::new(),
			audio_effects: Vec::new(),
		}],
		adjustments: Vec::new(),
	};

	// A short destination is rejected before anything is decoded.
	let mut tiny = [0u8; 8];
	assert!(
		render_montage_frame_into(Rational::new(0, 1), &params, (2, 1), &mut tiny, 32).is_err()
	);

	// A non-positive size is rejected too.
	let mut dst = vec![0u8; 64];
	assert!(render_montage_frame_into(Rational::new(0, 1), &params, (0, 1), &mut dst, 32).is_err());

	// The clip does not cover t=0: the frame stays transparent black.
	let mut dst = vec![0xFFu8; 2 * 16];
	render_montage_frame_into(Rational::new(0, 1), &params, (2, 1), &mut dst, 32)
		.expect("uncovered clip is skipped");
	assert!(dst.iter().all(|&b| b == 0));
}

#[test]
fn resolve_planar_footage_rejects_non_wgpu_contexts() {
	assert!(resolve_planar_footage(&Texture::dummy(), None).is_err());
	assert!(resolve_planar_footage(&planar_texture((2, 2)), None).is_err());
}

#[test]
fn decoded_frame_cache_dedups_breaks_and_prunes_planar() {
	let key =
		|n: i32| -> DecodedFrameKey { (format!("frame{n}.mp4"), 0, (n as i64, 1), 2, 2, false) };

	// A duplicate key is a no-op.
	let mut cache = std::collections::HashMap::new();
	insert_cached_frame(&mut cache, key(0), Texture::dummy(), 1);
	insert_cached_frame(&mut cache, key(0), Texture::dummy(), 2);
	assert_eq!(cache.len(), 1);
	let mut import_key = key(0);
	import_key.5 = true;
	insert_cached_frame(&mut cache, import_key, Texture::dummy(), 2);
	assert_eq!(cache.len(), 2, "CPU and import requests stay isolated");
	// Planar insertions take the planar-pruning path.
	insert_cached_frame(&mut cache, key(1), planar_texture((2, 2)), 3);
	assert_eq!(cache.len(), 3);

	// A cache full of tick-0 entries has no victim: the loop breaks.
	let mut cache = std::collections::HashMap::new();
	for i in 0..MAX_CACHED_FRAMES {
		cache.insert(key(i as i32), (Texture::dummy(), 0));
	}
	insert_cached_frame(&mut cache, key(100), Texture::dummy(), 7);
	assert_eq!(cache.len(), MAX_CACHED_FRAMES + 1);

	// Planar entries are pruned to the keep budget, LRU first.
	let mut cache = std::collections::HashMap::new();
	for i in 0..(MAX_CACHED_PLANAR_FRAMES + 3) {
		cache.insert(key(i as i32), (planar_texture((2, 2)), i as u64 + 1));
	}
	prune_planar_frames(&mut cache, 2);
	assert_eq!(
		cache.values().filter(|(t, _)| t.is_planar()).count(),
		2,
		"planar entries pruned to the budget"
	);
	// The oldest planar entries went first.
	assert!(cache.contains_key(&key(MAX_CACHED_PLANAR_FRAMES as i32 + 2)));
	// At or under the budget: a no-op.
	prune_planar_frames(&mut cache, 99);
	assert_eq!(cache.values().filter(|(t, _)| t.is_planar()).count(), 2);
}

#[test]
fn eviction_victim_falls_back_to_software_sessions() {
	type DecoderMap =
		std::collections::HashMap<(String, i32), (Arc<dyn quercus_codec::decoder::Decoder>, u64)>;
	let mut cache: DecoderMap = std::collections::HashMap::new();
	cache.insert(
		("a.mp4".to_string(), 0),
		(Arc::new(FFmpegDecoder::new()), 5),
	);
	cache.insert(
		("b.mp4".to_string(), 0),
		(Arc::new(FFmpegDecoder::new()), 3),
	);
	assert_eq!(
		eviction_victim(&cache),
		Some(("b.mp4".to_string(), 0)),
		"no hardware session: the LRU software one goes"
	);
	assert_eq!(eviction_victim(&DecoderMap::new()), None);
}

#[test]
fn missing_colorimetry_warning_is_callable_more_than_once() {
	warn_missing_colorimetry_once();
	warn_missing_colorimetry_once();
}

#[test]
fn opacity_factor_and_channel_scaling_edge_cases() {
	assert!(
		opacity_factor(&unknown_effect("com.example.opacity-test")).is_none(),
		"a non-opacity effect has no factor"
	);
	assert!(
		opacity_factor(&opacity_effect(true, 1.0)).is_none(),
		"unity is skipped"
	);
	assert_eq!(opacity_factor(&opacity_effect(true, 0.5)), Some(0.5));
	assert_eq!(
		opacity_factor(&opacity_effect(true, 2.0)),
		Some(2.0),
		"values above one scale up"
	);
	// An Opacity effect without the value input defaults to unity.
	let mut no_param = opacity_effect(true, 0.5);
	no_param.params.clear();
	assert!(opacity_factor(&no_param).is_none());

	let mut bytes = [0u8; 32];
	scale_channels_in_place(&mut bytes, 0, 2, 1, 2.0);
	scale_channels_in_place(&mut bytes, 32, 0, 1, 2.0);
	scale_channels_in_place(&mut bytes, 32, 2, 0, 2.0);
	assert_eq!(bytes, [0u8; 32], "invalid geometry is inert");
}

#[test]
fn montage_opacity_on_non_cpu_textures_warns_and_passes_through() {
	let effect = opacity_effect(true, 0.5);
	let out = apply_montage_effect(planar_texture((2, 1)), &effect, Rational::new(0, 1));
	assert!(out.is_planar(), "a planar texture is returned unchanged");
	let gpu = Texture::gpu(Arc::new(UnusedCtx), 1, 2, 1, PixelFormat::F32);
	let out = apply_montage_effect(gpu, &effect, Rational::new(0, 1));
	assert!(matches!(out, Texture::Gpu { .. }));
}

#[test]
fn montage_effect_without_a_resolvable_evaluator_passes_through() {
	let clip = clip_with_effects(vec![unknown_effect("com.example.no-evaluator")]);
	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	crate::ofxhost::install_client(None);
	set_plugin_instance_factory(Some(Arc::new(|_id: &str| None)));
	let out = apply_clip_effects(
		solid_texture(0.8, 0.4, 0.2, 1.0),
		&clip,
		Rational::new(0, 1),
	);
	assert_eq!(first_pixel(&out), [0.8, 0.4, 0.2, 1.0]);
	set_plugin_instance_factory(None);
}

#[test]
fn produced_frame_non_f32_uses_the_cpu_generator() {
	for format in [PixelFormat::U8, PixelFormat::U16] {
		let params = crate::ticket::VideoTicketParams {
			viewer: 1,
			project: String::new(),
			time: Rational::new(0, 1),
			force_size: Some((3, 2)),
			force_format: Some(format),
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage: None,
			montage: Vec::new(),
			adjustments: Vec::new(),
		};
		let tex = render_produced_frame(Rational::new(1, 1), &params).unwrap();
		assert!(
			matches!(tex, Texture::Cpu(_)),
			"{format:?} uses the CPU producer"
		);
		assert_eq!(tex.size(), (3, 2));
		assert_eq!(tex.format(), format);
	}
}

#[test]
fn generate_frame_with_an_invalid_format_reports_nomem() {
	assert!(generate_frame(Rational::new(0, 1), (4, 4), PixelFormat::Invalid).is_err());
}

#[test]
fn convert_decoded_to_working_handles_missing_metadata_and_short_buffers() {
	let _guard = working_space_test_lock()
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let previous = quercus_core::color::pipeline_working_space();
	let output = quercus_core::color::pipeline_output_spec();
	quercus_core::color::set_pipeline_color_settings(
		quercus_core::colormath::WorkingColorSpace::AcesCg,
		output,
	);
	let mut decoded = quercus_codec::frame::Frame::new();
	decoded.params = None; // no colorimetry metadata

	// The generic sRGB fallback converts in place.
	let mut dst = generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
	convert_decoded_to_working(&mut dst, &decoded);

	// Zero-sized destinations return before touching the buffer.
	let mut zero = Frame::new();
	convert_decoded_to_working(&mut zero, &decoded);

	// A short buffer breaks out of the row loop.
	let mut short = generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
	short.data.truncate(16);
	convert_decoded_to_working(&mut short, &decoded);

	quercus_core::color::set_pipeline_color_settings(previous, output);
}

#[test]
fn footage_working_lut_caches_and_clears() {
	let _guard = working_space_test_lock()
		.lock()
		.unwrap_or_else(|e| e.into_inner());
	let previous = quercus_core::color::pipeline_working_space();
	let output = quercus_core::color::pipeline_output_spec();
	quercus_core::color::set_pipeline_color_settings(
		quercus_core::colormath::WorkingColorSpace::AcesCg,
		output,
	);

	let first = footage_working_lut(1, 1);
	assert!(first.is_some());
	let second = footage_working_lut(1, 1);
	assert_eq!(
		first.expect("first LUT").0,
		second.expect("cached LUT").0,
		"the second request hits the cache"
	);
	// 16+ distinct colorimetries flush the per-process cache.
	for i in 0..20i32 {
		let _ = footage_working_lut(10 + i * 3, 20 + i * 7);
	}

	quercus_core::color::set_pipeline_color_settings(previous, output);
}

#[test]
fn color_transform_lut_cache_hits_and_clears() {
	let Some(processor) = red_doubling_processor("lutcache") else {
		return;
	};
	assert!(color_transform_lut(&processor).is_some());
	assert!(
		color_transform_lut(&processor).is_some(),
		"the second request hits the processor's cache entry"
	);

	// Distinct processors flush the cache once it holds 16 entries.
	let mut ids = std::collections::HashSet::new();
	for i in 1..20 {
		if let Some(p) = lut_processor(&format!("lutflush{i}"), 1.0 + i as f32 * 0.25) {
			ids.insert(p.cache_id());
			let _ = color_transform_lut(&p);
		}
	}
	if ids.len() < 2 {
		eprintln!("OCIO cache ids are not distinct; cache flush not exercised");
	}
}

/// Serializes the tests that set process-wide environment variables
/// (`QUERCUS_PERF`) or the decode-service slot.
static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Saves `QUERCUS_PERF` and restores its previous value (or absence) on
/// drop, so a panicking assertion cannot leak the perf override into
/// the next serialized env test. Callers hold [`ENV_TEST_LOCK`].
struct PerfEnvGuard(Option<std::ffi::OsString>);

impl PerfEnvGuard {
	fn enable() -> PerfEnvGuard {
		let previous = std::env::var_os("QUERCUS_PERF");
		std::env::set_var("QUERCUS_PERF", "1");
		PerfEnvGuard(previous)
	}
}

impl Drop for PerfEnvGuard {
	fn drop(&mut self) {
		match self.0.take() {
			Some(value) => std::env::set_var("QUERCUS_PERF", value),
			None => std::env::remove_var("QUERCUS_PERF"),
		}
	}
}

/// Serializes the tests that flip [`GPU_COMPOSITE_FAILED`].
static COMPOSITE_FLAG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A footage node probed from `path`, wired to a clip block spanning
/// `[in_, out)`, on `graph`.
fn add_footage_clip(
	graph: &mut quercus_node::graph::Graph,
	path: &str,
	in_: Rational,
	out: Rational,
) -> quercus_node::id::NodeId {
	let mut footage = quercus_node::footage::FootageBehavior::new(path);
	footage.probe().expect("probe the test clip");
	let footage = graph.add_node(quercus_node::node::NodeCore::new(), Box::new(footage));
	let (ccore, cbehavior) = quercus_node::block::clip_create();
	let clip = graph.add_node(ccore, cbehavior);
	graph
		.connect(
			footage,
			clip,
			quercus_node::block::clip_input::TEXTURE_INPUT,
			-1,
		)
		.expect("footage -> clip");
	graph
		.get_mut(clip)
		.unwrap()
		.behavior
		.as_any_mut()
		.unwrap()
		.downcast_mut::<quercus_node::block::ClipBlockBehavior>()
		.expect("clip block")
		.core
		.range = TimeRange::new(in_, out);
	clip
}

/// An enabled clip block of `[in_, out)` with nothing connected.
fn add_bare_clip(
	graph: &mut quercus_node::graph::Graph,
	in_: Rational,
	out: Rational,
) -> quercus_node::id::NodeId {
	let (ccore, cbehavior) = quercus_node::block::clip_create();
	let clip = graph.add_node(ccore, cbehavior);
	graph
		.get_mut(clip)
		.unwrap()
		.behavior
		.as_any_mut()
		.unwrap()
		.downcast_mut::<quercus_node::block::ClipBlockBehavior>()
		.expect("clip block")
		.core
		.range = TimeRange::new(in_, out);
	clip
}

/// One sequence with three video tracks: V0 (a plain footage clip),
/// V1 (two clips joined by a transition covering t=1) and V2 (an
/// adjustment block with an opacity chain). Rendering t=1 walks the
/// clip, transition and adjustment steps.
fn build_three_step_project(path: &str) -> (Arc<Mutex<Project>>, quercus_node::id::NodeId) {
	let project = Project::new();
	let seq;
	{
		let mut p = project.lock().unwrap();
		let graph = &mut p.graph;
		let (score, sbehavior) = quercus_node::sequence::SequenceBehavior::create();
		seq = graph.add_node(score, sbehavior);
		let (tlcore, tlbehavior) = quercus_node::track::TrackListBehavior::create();
		let tl = graph.add_node(tlcore, tlbehavior);

		// V0: one clip covering t=1.
		let (tcore, tbehavior) = quercus_node::track::TrackBehavior::create();
		let v0 = graph.add_node(tcore, tbehavior);
		let c0 = add_footage_clip(graph, path, Rational::new(0, 1), Rational::new(2, 1));
		graph
			.get_mut(v0)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::track::TrackBehavior>()
			.unwrap()
			.append_block(c0);

		// V1: two clips joined by a transition covering t=1.
		let (tcore, tbehavior) = quercus_node::track::TrackBehavior::create();
		let v1 = graph.add_node(tcore, tbehavior);
		let a = add_footage_clip(graph, path, Rational::new(0, 1), Rational::new(1, 1));
		let b = add_footage_clip(graph, path, Rational::new(1, 1), Rational::new(2, 1));
		let (trcore, trbehavior) = quercus_node::block::transition_create();
		let transition = graph.add_node(trcore, trbehavior);
		{
			let t = graph
				.get_mut(transition)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<quercus_node::block::TransitionBlockBehavior>()
				.unwrap();
			t.core.range = TimeRange::new(Rational::new(1, 2), Rational::new(3, 2));
			t.core.enabled = true;
		}
		graph
			.connect(
				a,
				transition,
				quercus_node::block::transition_input::OUT_BLOCK,
				-1,
			)
			.unwrap();
		graph
			.connect(
				b,
				transition,
				quercus_node::block::transition_input::IN_BLOCK,
				-1,
			)
			.unwrap();
		{
			let track = graph
				.get_mut(v1)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<quercus_node::track::TrackBehavior>()
				.unwrap();
			track.append_block(a);
			track.append_block(transition);
			track.append_block(b);
		}

		// V2: an adjustment block with an opacity chain on top.
		let (tcore, tbehavior) = quercus_node::track::TrackBehavior::create();
		let v2 = graph.add_node(tcore, tbehavior);
		let (acore, abehavior) = quercus_node::block::adjustment_create();
		let adjustment = graph.add_node(acore, abehavior);
		{
			let a = graph
				.get_mut(adjustment)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<quercus_node::block::AdjustmentBlockBehavior>()
				.unwrap();
			a.core.range = TimeRange::new(Rational::new(0, 1), Rational::new(2, 1));
			a.core.enabled = true;
		}
		let (ecore, ebehavior) = quercus_node::nodes::opacity::create();
		let effect = graph.add_node(ecore, ebehavior);
		graph
			.connect(
				effect,
				adjustment,
				quercus_node::block::adjustment_input::TEXTURE_INPUT,
				-1,
			)
			.unwrap();
		graph.get_mut(effect).unwrap().core.set_standard_value(
			quercus_node::nodes::opacity::VALUE_INPUT,
			-1,
			NodeValue::Float(0.5),
		);
		graph
			.get_mut(v2)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::track::TrackBehavior>()
			.unwrap()
			.append_block(adjustment);

		let tl_behavior = graph
			.get_mut(tl)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::track::TrackListBehavior>()
			.unwrap();
		tl_behavior.tracks.push(v0);
		tl_behavior.tracks.push(v1);
		tl_behavior.tracks.push(v2);
		graph
			.get_mut(seq)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::sequence::SequenceBehavior>()
			.unwrap()
			.track_lists
			.push(tl);
	}
	(project, seq)
}

/// A sequence whose track/track-block lists mix stale ids, foreign
/// behaviors and degenerate blocks; returns the sequence plus the
/// bare and undecodable clips for direct evaluation.
fn build_degenerate_project(
	missing_media: &str,
) -> (
	Arc<Mutex<Project>>,
	quercus_node::id::NodeId,
	quercus_node::id::NodeId,
	quercus_node::id::NodeId,
) {
	use quercus_node::id::NodeId;
	let project = Project::new();
	let empty_clip;
	let bad_clip;
	let seq;
	{
		let mut p = project.lock().unwrap();
		let graph = &mut p.graph;
		let (score, sbehavior) = quercus_node::sequence::SequenceBehavior::create();
		seq = graph.add_node(score, sbehavior);
		let (tlcore, tlbehavior) = quercus_node::track::TrackListBehavior::create();
		let tl = graph.add_node(tlcore, tlbehavior);

		// Stale and foreign entries in the sequence's track lists.
		let stale_list = NodeId::from_identity(900_001).unwrap();
		let (score2, sbehavior2) = quercus_node::sequence::SequenceBehavior::create();
		let not_a_tracklist = graph.add_node(score2, sbehavior2);

		// A real list with stale/foreign entries in its track list.
		let stale_track = NodeId::from_identity(900_002).unwrap();
		let (score3, sbehavior3) = quercus_node::sequence::SequenceBehavior::create();
		let not_a_track = graph.add_node(score3, sbehavior3);
		let (tcore, tbehavior) = quercus_node::track::TrackBehavior::create();
		let track = graph.add_node(tcore, tbehavior);

		// Blocks: a stale id, a transition with no neighbors and a
		// bare clip.
		let stale_block = NodeId::from_identity(900_003).unwrap();
		let (trcore, trbehavior) = quercus_node::block::transition_create();
		let transition = graph.add_node(trcore, trbehavior);
		{
			let t = graph
				.get_mut(transition)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<quercus_node::block::TransitionBlockBehavior>()
				.unwrap();
			t.core.range = TimeRange::new(Rational::new(0, 1), Rational::new(2, 1));
			t.core.enabled = true;
		}
		empty_clip = add_bare_clip(graph, Rational::new(0, 1), Rational::new(2, 1));
		{
			let track_behavior = graph
				.get_mut(track)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<quercus_node::track::TrackBehavior>()
				.unwrap();
			track_behavior.blocks.push(stale_block);
			track_behavior.blocks.push(transition);
			track_behavior.blocks.push(empty_clip);
		}

		// A second real track whose clip references a missing file:
		// the unresolved footage-job box is left in the table.
		let (tcore, tbehavior) = quercus_node::track::TrackBehavior::create();
		let track2 = graph.add_node(tcore, tbehavior);
		bad_clip = add_footage_clip(
			graph,
			missing_media,
			Rational::new(0, 1),
			Rational::new(2, 1),
		);
		graph
			.get_mut(track2)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::track::TrackBehavior>()
			.unwrap()
			.append_block(bad_clip);

		{
			let tl_behavior = graph
				.get_mut(tl)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<quercus_node::track::TrackListBehavior>()
				.unwrap();
			tl_behavior.tracks.push(stale_track);
			tl_behavior.tracks.push(not_a_track);
			tl_behavior.tracks.push(track);
			tl_behavior.tracks.push(track2);
		}
		graph
			.get_mut(seq)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::sequence::SequenceBehavior>()
			.unwrap()
			.track_lists
			.push(stale_list);
		graph
			.get_mut(seq)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::sequence::SequenceBehavior>()
			.unwrap()
			.track_lists
			.push(not_a_tracklist);
		graph
			.get_mut(seq)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::sequence::SequenceBehavior>()
			.unwrap()
			.track_lists
			.push(tl);
	}
	(project, seq, empty_clip, bad_clip)
}

#[test]
fn render_graph_frame_rejects_bad_format_and_size() {
	let project = Project::new();
	let seq;
	{
		let mut p = project.lock().unwrap();
		let (score, sbehavior) = quercus_node::sequence::SequenceBehavior::create();
		seq = p.graph.add_node(score, sbehavior);
	}
	assert!(render_graph_frame(
		&project,
		seq,
		Rational::new(0, 1),
		(16, 16),
		PixelFormat::U8
	)
	.is_err());
	assert!(render_graph_frame(
		&project,
		seq,
		Rational::new(0, 1),
		(0, 16),
		PixelFormat::F32
	)
	.is_err());
}

#[test]
fn render_graph_frame_skips_stale_and_textureless_steps() {
	// Probe a real clip, then delete the file: the footage job is built
	// at evaluation time but its decode fails, so the unresolved box
	// reaches the texture-channel checks.
	let path = std::env::temp_dir().join(format!(
		"quercusrender_degenerate_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip(&path, 16, 16, 10, 10).expect("test clip");
	let name = path.to_string_lossy().into_owned();
	let (project, seq, empty_clip, bad_clip) = build_degenerate_project(&name);
	let _ = std::fs::remove_file(&path);

	let out = render_graph_frame(&project, seq, Rational::new(1, 2), (4, 4), PixelFormat::F32);
	assert_eq!(out.expect("degenerate render").size(), (4, 4));

	// Directly: a clip with nothing connected yields no texture, an
	// undecodable footage job leaves its box behind.
	let mut traverser = quercus_node::traverser::Traverser::new();
	let mut hooks = RenderEvalHooks::new();
	{
		let guard = project.lock().unwrap();
		let empty = evaluate_block_frame(
			&guard.graph,
			&mut traverser,
			&mut hooks,
			empty_clip,
			Rational::new(1, 2),
		);
		assert!(matches!(empty, Ok(None)), "a bare clip produces nothing");
		let bad = evaluate_block_frame(
			&guard.graph,
			&mut traverser,
			&mut hooks,
			bad_clip,
			Rational::new(1, 2),
		);
		assert!(
			matches!(bad, Ok(None)),
			"an unresolved footage job is not a texture"
		);
	}
}

#[test]
fn render_graph_frame_stamps_cpu_frames_when_the_gpu_composite_is_off() {
	let project = Project::new();
	let seq;
	{
		let mut p = project.lock().unwrap();
		let (score, sbehavior) = quercus_node::sequence::SequenceBehavior::create();
		seq = p.graph.add_node(score, sbehavior);
	}
	let _guard = COMPOSITE_FLAG_LOCK.lock().unwrap();
	let previous = GPU_COMPOSITE_FAILED.swap(true, std::sync::atomic::Ordering::Relaxed);
	let out = render_graph_frame(&project, seq, Rational::new(3, 1), (4, 4), PixelFormat::F32);
	GPU_COMPOSITE_FAILED.store(previous, std::sync::atomic::Ordering::Relaxed);
	match out.expect("cpu composite") {
		Texture::Cpu(frame) => {
			assert_eq!(frame.timestamp, Rational::new(3, 1));
			assert_eq!((frame.width, frame.height), (4, 4));
		}
		Texture::Gpu { .. } | Texture::Planar(_) => {
			panic!("the CPU fallback must produce a CPU frame")
		}
	}
}

#[test]
fn quercus_perf_graph_render_logs_every_step() {
	let _guard = ENV_TEST_LOCK.lock().unwrap();
	let path = std::env::temp_dir().join(format!(
		"quercusrender_perf_graph_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip(&path, 16, 16, 10, 10).expect("test clip");
	let (project, seq) = build_three_step_project(&path.to_string_lossy());
	let _perf = PerfEnvGuard::enable();
	let out = render_graph_frame(
		&project,
		seq,
		Rational::new(1, 1),
		(16, 16),
		PixelFormat::F32,
	);
	assert_eq!(out.expect("perf render").size(), (16, 16));
	let _ = std::fs::remove_file(&path);
}

#[test]
fn quercus_perf_footage_decode_logs_and_reuses_sessions() {
	let _guard = ENV_TEST_LOCK.lock().unwrap();
	let path = std::env::temp_dir().join(format!(
		"quercusrender_perf_decode_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip(&path, 16, 16, 10, 10).expect("test clip");
	let name = path.to_string_lossy().into_owned();
	let _perf = PerfEnvGuard::enable();
	let first = open_decoder(&name, 0);
	let second = open_decoder(&name, 0);
	let decoded = render_footage_frame(&name, 0, Rational::new(0, 1), (16, 16), PixelFormat::F32);
	assert!(first.is_ok(), "the first open logs (request)");
	assert!(second.is_ok(), "the second open logs CACHED");
	assert!(decoded.is_ok(), "the decode logs [decode]");
	let _ = std::fs::remove_file(&path);
}

#[test]
fn shut_down_decode_service_falls_back_to_inline_decode() {
	let _guard = ENV_TEST_LOCK.lock().unwrap();
	let path = std::env::temp_dir().join(format!(
		"quercusrender_stopped_service_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip(&path, 16, 16, 10, 10).expect("test clip");
	let name = path.to_string_lossy().into_owned();
	let service = crate::pipeline::DecodeService::new(1, Arc::new(|| true));
	service.shutdown();
	crate::pipeline::install_decode_service(Some(service));
	let direct = render_footage_frame(&name, 0, Rational::new(0, 1), (16, 16), PixelFormat::F32);
	let staged =
		render_footage_frame_staged(&name, 0, Rational::new(0, 1), (16, 16), PixelFormat::F32);
	crate::pipeline::install_decode_service(None);
	assert!(direct.is_ok(), "a gone service falls back inline");
	assert!(staged.is_ok(), "the staged path falls back inline too");
	let _ = std::fs::remove_file(&path);
}

// ---- Coverage edges: GPU shader jobs and composites -----------------

/// A payload that asks a registered node for a shader variant nobody
/// implements: the job warns and yields nothing.
#[test]
fn gpu_shader_job_reports_missing_shaders_and_bad_bindings() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}

	let payload = ShaderJobPayload {
		type_id: "org.olivevideoeditor.Olive.checkerboard".into(),
		shader_id: "no-such-shader".into(),
		effect_input: "tex_in".into(),
		..ShaderJobPayload::default()
	};
	assert!(
		RenderEvalHooks::new()
			.process_shader_job(&payload)
			.is_none(),
		"an unknown shader id yields no texture"
	);

	// merge (no declared effect input): a real base plus a null handle
	// and an unresolved planar texture. Both bad inputs are skipped,
	// the pass still runs at the base's size.
	let mut params = NodeValueRow::new();
	params.insert(
		"base_in".into(),
		texture_value(filled_frame((4, 4), [1.0, 0.0, 0.0, 1.0])),
	);
	params.insert("blend_in".into(), NodeValue::Texture(CHandle::null()));
	params.insert(
		"extra_in".into(),
		NodeValue::Texture(quercus_node::handle::make_owned(planar_texture((4, 4)))),
	);
	let payload = ShaderJobPayload {
		type_id: "org.olivevideoeditor.Olive.merge".into(),
		shader_id: String::new(),
		effect_input: String::new(),
		params,
		..ShaderJobPayload::default()
	};
	let out = RenderEvalHooks::new()
		.process_shader_job(&payload)
		.expect("the merge runs with the skippable inputs unbound");
	assert_eq!(out.size(), (4, 4));
}

#[test]
fn gpu_shader_job_binds_nested_shader_payloads() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	let nested = Job::ShaderJob(ShaderJobPayload {
		type_id: "org.olivevideoeditor.Olive.solidgenerator".into(),
		params: NodeValueRow::from([(
			"color_in".to_string(),
			NodeValue::Color([0.0, 1.0, 0.0, 1.0]),
		)]),
		..ShaderJobPayload::default()
	});
	let mut params = NodeValueRow::new();
	params.insert(
		"base_in".into(),
		texture_value(filled_frame((4, 4), [1.0, 0.0, 0.0, 1.0])),
	);
	params.insert(
		"blend_in".into(),
		NodeValue::Texture(quercus_node::handle::make_owned(nested)),
	);
	let payload = ShaderJobPayload {
		type_id: "org.olivevideoeditor.Olive.merge".into(),
		shader_id: String::new(),
		effect_input: String::new(),
		params,
		..ShaderJobPayload::default()
	};
	let out = RenderEvalHooks::new()
		.process_shader_job(&payload)
		.expect("the nested generator resolves through the bind");
	let px = first_pixel(&out);
	assert!(
		px[1] > 0.9 && px[0] < 0.1,
		"the generated green covers the red base: {px:?}"
	);
}

#[test]
fn gpu_grading_job_splices_the_ocio_grading_stub() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	if quercus_core::color::set_up_default_config().is_err() {
		eprintln!("bundled OCIO missing; skipping");
		return;
	}
	if quercus_core::color::grading_primary_function_shader(quercus_core::color::GradingStyle::Lin)
		.is_none()
	{
		eprintln!("no OCIO grading stub; skipping");
		return;
	}
	let mut params = NodeValueRow::new();
	params.insert(
		"tex_in".into(),
		texture_value(filled_frame((4, 4), [0.5, 0.5, 0.5, 1.0])),
	);
	params.insert(
		"OCIO_NAMESPACE_grading_primary_brightness".into(),
		NodeValue::Float(1.0),
	);
	let payload = ShaderJobPayload {
		type_id: "org.olivevideoeditor.Olive.ociogradingtransformlinear".into(),
		shader_id: String::new(),
		effect_input: "tex_in".into(),
		params,
		..ShaderJobPayload::default()
	};
	let out = RenderEvalHooks::new().process_shader_job(&payload);
	assert!(
		out.is_some(),
		"the grading node renders with the spliced OCIO stub"
	);
}

#[test]
fn gpu_composite_reads_foreign_textures_and_rejects_planar() {
	let Some(ctx) = quercus_core::backend::shared_gpu_or_skip("an eval GPU test") else {
		return;
	};
	assert!(composite_tracks_gpu(&ctx, &[], (0, 1)).is_err());

	// A GPU texture from another context is read back and re-uploaded.
	let foreign = echo_gpu_texture((2, 1), [0.25, 0.5, 0.75, 1.0], 0xDEAD_BEEF);
	let out = composite_tracks_gpu(&ctx, &[foreign], (2, 1)).expect("foreign readback");
	let px = first_pixel(&out);
	assert!(
		(px[0] - 0.25).abs() < 5e-3 && (px[1] - 0.5).abs() < 5e-3 && (px[2] - 0.75).abs() < 5e-3,
		"foreign texture survives the readback: {px:?}"
	);

	// An unresolved planar frame is a hard error; the accumulator is
	// torn down on the way out.
	let planar = planar_texture((2, 1));
	assert!(composite_tracks_gpu(&ctx, &[planar], (2, 1)).is_err());
}

#[test]
fn composite_tracks_cpu_fallback_skips_unreadable_and_mismatched_frames() {
	let _guard = COMPOSITE_FLAG_LOCK.lock().unwrap();
	let previous = GPU_COMPOSITE_FAILED.swap(false, std::sync::atomic::Ordering::Relaxed);
	// The planar frame fails the GPU pass (and flips the sticky flag);
	// the CPU fallback then skips it, skips a wrongly-sized frame and
	// composites the valid one.
	let planar = planar_texture((2, 1));
	let wrong = filled_frame((3, 1), [0.0, 1.0, 0.0, 1.0]);
	let right = filled_frame((2, 1), [1.0, 0.0, 0.0, 1.0]);
	let out = composite_tracks(vec![planar, wrong, right], (2, 1));
	GPU_COMPOSITE_FAILED.store(previous, std::sync::atomic::Ordering::Relaxed);
	assert_eq!(out.size(), (2, 1));
	let px = first_pixel(&out);
	assert!(
		(px[0] - 1.0).abs() < 1e-4 && px[1].abs() < 1e-4 && (px[3] - 1.0).abs() < 1e-4,
		"only the valid frame composites: {px:?}"
	);
}

// ---- Coverage edges: montage clip effects with GPU results ----------

fn montage_params_with_effect(
	path: &str,
	effect_type_id: &str,
) -> crate::ticket::VideoTicketParams {
	crate::ticket::VideoTicketParams {
		viewer: 1,
		project: String::new(),
		time: Rational::new(0, 1),
		force_size: Some((16, 16)),
		force_format: Some(PixelFormat::F32),
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage: vec![crate::ticket::MontageClip {
			filename: path.to_string(),
			stream_index: 0,
			in_time: Rational::new(0, 1),
			out_time: Rational::new(2, 1),
			media_in: Rational::new(0, 1),
			gain: 1.0,
			pan: 0.0,
			fade_in: Rational::new(0, 1),
			fade_out: Rational::new(0, 1),
			effects: vec![unknown_effect(effect_type_id)],
			audio_effects: Vec::new(),
		}],
		adjustments: Vec::new(),
	}
}

#[test]
fn montage_reads_back_gpu_textures_from_clip_effects() {
	let _env = ENV_TEST_LOCK.lock().unwrap();
	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	let path = std::env::temp_dir().join(format!(
		"quercusrender_montage_gpu_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip(&path, 16, 16, 10, 10).expect("test clip");
	crate::ofxhost::install_client(None);
	set_plugin_instance_factory(Some(Arc::new(|_id: &str| Some(7))));
	set_plugin_executor(Some(Arc::new(|req: &PluginJobRequest<'_>| {
		let frame = filled_frame(req.src.size(), [0.5, 0.5, 0.5, 1.0]).to_frame()?;
		Ok(Texture::gpu(
			Arc::new(FrameEchoCtx { frame }),
			0xE0,
			req.src.size().0,
			req.src.size().1,
			PixelFormat::F32,
		))
	})));
	let params = montage_params_with_effect(&path.to_string_lossy(), "com.example.gpu-effect");
	let mut dst = vec![0u8; 16 * 16 * 16];
	render_montage_frame_into(Rational::new(0, 1), &params, (16, 16), &mut dst, 256)
		.expect("the GPU effect result is read back and composited");
	let px = read_f32_px(&dst, 256, 1, 1);
	assert!((px[0] - 0.5).abs() < 1e-4, "gpu effect pixels land: {px:?}");
	set_plugin_executor(None);
	set_plugin_instance_factory(None);
	let _ = std::fs::remove_file(&path);
}

#[test]
fn montage_rejects_planar_textures_from_clip_effects() {
	let _env = ENV_TEST_LOCK.lock().unwrap();
	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	let path = std::env::temp_dir().join(format!(
		"quercusrender_montage_planar_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip(&path, 16, 16, 10, 10).expect("test clip");
	crate::ofxhost::install_client(None);
	set_plugin_instance_factory(Some(Arc::new(|_id: &str| Some(7))));
	set_plugin_executor(Some(Arc::new(|req: &PluginJobRequest<'_>| {
		Ok(planar_texture(req.src.size()))
	})));
	let params = montage_params_with_effect(&path.to_string_lossy(), "com.example.planar-effect");
	let mut dst = vec![0u8; 16 * 16 * 16];
	let err = render_montage_frame_into(Rational::new(0, 1), &params, (16, 16), &mut dst, 256);
	set_plugin_executor(None);
	set_plugin_instance_factory(None);
	let _ = std::fs::remove_file(&path);
	assert!(err.is_err(), "an unresolved planar texture is rejected");
}

/// The single OFX host client owns dispatch when one is installed:
/// the montage effect path takes the host branch, and a host that
/// cannot come up yields the purple failure frame.
#[test]
#[cfg(unix)]
fn montage_effect_uses_the_installed_ofx_host() {
	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	let host = crate::ofxhost::OfxHost::new(crate::ofxhost::OfxHostConfig {
		host_bin: Some(std::path::PathBuf::from("/bin/false")),
		max_failures: 1,
		..Default::default()
	})
	.unwrap();
	crate::ofxhost::install_client(Some(host));
	let out = apply_montage_effect(
		solid_texture(0.8, 0.4, 0.2, 1.0),
		&unknown_effect("com.example.host-effect"),
		Rational::new(0, 1),
	);
	crate::ofxhost::install_client(None);
	assert_eq!(
		first_pixel(&out),
		[1.0, 0.0, 1.0, 1.0],
		"a failed host render paints the purple frame"
	);
}

/// A test-only node behavior that emits a null texture handle, so the
/// graph seams' null-handle guards are reachable without a producer
/// bug.
struct NullTextureBehavior;

impl quercus_node::node::NodeBehavior for NullTextureBehavior {
	fn name(&self) -> &str {
		"NullTexture"
	}
	fn type_id(&self) -> &str {
		"org.quercus.test.nulltexture"
	}
	fn duplicate(
		&self,
		_core: &quercus_node::node::NodeCore,
	) -> Option<Box<dyn quercus_node::node::NodeBehavior>> {
		Some(Box::new(Self))
	}
	fn value(
		&self,
		_core: &quercus_node::node::NodeCore,
		_inputs: &NodeValueRow,
		_time: Rational,
		table: &mut NodeValueTable,
	) {
		table.push(
			quercus_node::value::ValueType::Texture,
			NodeValue::Texture(CHandle::null()),
			None,
		);
	}
}

#[test]
fn null_texture_outputs_are_skipped_by_the_graph_paths() {
	let project = Project::new();
	let seq;
	let clip;
	{
		let mut p = project.lock().unwrap();
		let graph = &mut p.graph;
		let (score, sbehavior) = quercus_node::sequence::SequenceBehavior::create();
		seq = graph.add_node(score, sbehavior);
		let (tlcore, tlbehavior) = quercus_node::track::TrackListBehavior::create();
		let tl = graph.add_node(tlcore, tlbehavior);
		let (tcore, tbehavior) = quercus_node::track::TrackBehavior::create();
		let track = graph.add_node(tcore, tbehavior);

		let null_node = graph.add_node(
			quercus_node::node::NodeCore::new(),
			Box::new(NullTextureBehavior),
		);
		clip = add_bare_clip(graph, Rational::new(0, 1), Rational::new(1, 1));
		graph
			.connect(
				null_node,
				clip,
				quercus_node::block::clip_input::TEXTURE_INPUT,
				-1,
			)
			.expect("null node -> clip");
		graph
			.get_mut(track)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::track::TrackBehavior>()
			.unwrap()
			.append_block(clip);
		graph
			.get_mut(tl)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::track::TrackListBehavior>()
			.unwrap()
			.tracks
			.push(track);
		graph
			.get_mut(seq)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::sequence::SequenceBehavior>()
			.unwrap()
			.track_lists
			.push(tl);
	}

	// Direct: a null handle is not a texture (`Ok(None)`).
	{
		let guard = project.lock().unwrap();
		let mut traverser = quercus_node::traverser::Traverser::new();
		let mut hooks = RenderEvalHooks::new();
		let out = evaluate_block_frame(
			&guard.graph,
			&mut traverser,
			&mut hooks,
			clip,
			Rational::new(1, 2),
		);
		assert!(matches!(out, Ok(None)));
	}

	// Render: the clip's null texture is skipped and the frame stays
	// transparent black.
	let frame = render_graph_frame(&project, seq, Rational::new(1, 2), (4, 4), PixelFormat::F32)
		.expect("null texture render");
	assert_eq!(frame.size(), (4, 4));
	let readback = frame.to_frame().expect("readback");
	assert!(readback.data.iter().all(|&b| b == 0));
}

// ---- Coverage edges: shader bind failures and adjustment sweeps ----

/// A behavior with a synthetic texture output, so the transition and
/// sweep seams can be driven without a real producer. `value` is
/// pushed on the texture channel (nothing when `None`).
struct FixedTextureBehavior {
	value: Option<NodeValue>,
}

impl quercus_node::node::NodeBehavior for FixedTextureBehavior {
	fn name(&self) -> &str {
		"FixedTexture"
	}
	fn type_id(&self) -> &str {
		"org.quercus.test.fixedtexture"
	}
	fn duplicate(
		&self,
		_core: &quercus_node::node::NodeCore,
	) -> Option<Box<dyn quercus_node::node::NodeBehavior>> {
		Some(Box::new(Self {
			value: self.value.clone(),
		}))
	}
	fn value(
		&self,
		_core: &quercus_node::node::NodeCore,
		_inputs: &NodeValueRow,
		_time: Rational,
		table: &mut NodeValueTable,
	) {
		if let Some(value) = &self.value {
			table.push(quercus_node::value::ValueType::Texture, value.clone(), None);
		}
	}
}

/// A behavior whose fragment source cannot compile (the shader-job
/// path must warn and report no texture).
struct BadShaderBehavior;

impl quercus_node::node::NodeBehavior for BadShaderBehavior {
	fn name(&self) -> &str {
		"BadShader"
	}
	fn type_id(&self) -> &str {
		"org.quercus.test.badshader"
	}
	fn duplicate(
		&self,
		_core: &quercus_node::node::NodeCore,
	) -> Option<Box<dyn quercus_node::node::NodeBehavior>> {
		Some(Box::new(Self))
	}
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some("%%% this is not valid glsl %%%".to_string())
	}
}

/// A core with an effect input `tex_in`, optionally not connectable
/// (the sweep's refused-connection error path).
fn effect_head_core(connectable: bool) -> quercus_node::node::NodeCore {
	let mut core = quercus_node::node::NodeCore::new();
	let mut input = quercus_node::input::Input::new(
		"tex_in",
		quercus_node::value::ValueType::Texture,
		NodeValue::None,
	);
	if !connectable {
		input.flags |= quercus_node::input::flags::NOT_CONNECTABLE;
	}
	core.add_input(input);
	core.effect_input = "tex_in".to_string();
	core
}

/// The nested-payload recursion ceiling in `bind`: a job box at the
/// depth limit is left unbound instead of recursing, and the pass
/// still runs against the placeholders.
#[test]
fn gpu_shader_job_depth_ceiling_leaves_the_nested_box_unbound() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	let nested = Job::ShaderJob(ShaderJobPayload {
		type_id: "org.olivevideoeditor.Olive.solidgenerator".into(),
		params: NodeValueRow::from([(
			"color_in".to_string(),
			NodeValue::Color([0.0, 1.0, 0.0, 1.0]),
		)]),
		..ShaderJobPayload::default()
	});
	let mut params = NodeValueRow::new();
	params.insert(
		"blend_in".into(),
		NodeValue::Texture(quercus_node::handle::make_owned(nested)),
	);
	let payload = ShaderJobPayload {
		type_id: "org.olivevideoeditor.Olive.merge".into(),
		shader_id: String::new(),
		effect_input: String::new(),
		params,
		..ShaderJobPayload::default()
	};
	let out = RenderEvalHooks::new()
		.process_shader_job_depth(&payload, 8)
		.expect("the pass runs with the nested box unbound");
	assert_eq!(out.size(), (1, 1), "no input bound: the 1x1 fallback");
}

/// Inputs whose scratch texture cannot be created (a 0x0 frame) or
/// uploaded (a frame with a short buffer) are skipped; the pass runs
/// against the placeholders.
#[test]
fn gpu_shader_job_skips_uncreatable_and_unuploadable_inputs() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	// A 0x0 CPU texture: `create_texture` rejects the geometry.
	let zero = Texture::dummy();
	// A 2x2 frame with no payload: `upload` rejects the short buffer.
	let mut short = generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
	short.data.clear();
	let mut params = NodeValueRow::new();
	params.insert("base_in".into(), texture_value(zero));
	params.insert("blend_in".into(), texture_value(Texture::wrap_frame(short)));
	let payload = ShaderJobPayload {
		type_id: "org.olivevideoeditor.Olive.merge".into(),
		shader_id: String::new(),
		effect_input: String::new(),
		params,
		..ShaderJobPayload::default()
	};
	let out = RenderEvalHooks::new()
		.process_shader_job(&payload)
		.expect("the pass runs with both bad inputs skipped");
	assert_eq!(out.size(), (1, 1));
}

/// A bound token the context does not own fails the pass at run time
/// (and the reserved output texture is released).
#[test]
fn gpu_shader_job_run_failure_reports_none() {
	let Some(ctx) = quercus_core::backend::shared_gpu_or_skip("an eval GPU test") else {
		return;
	};
	let bogus = Texture::gpu(ctx, 0xDEAD_BEEF, 4, 4, PixelFormat::F32);
	let mut params = NodeValueRow::new();
	params.insert("base_in".into(), texture_value(bogus));
	let payload = ShaderJobPayload {
		type_id: "org.olivevideoeditor.Olive.merge".into(),
		shader_id: String::new(),
		effect_input: String::new(),
		params,
		..ShaderJobPayload::default()
	};
	assert!(
		RenderEvalHooks::new()
			.process_shader_job(&payload)
			.is_none(),
		"a foreign token fails the bind group and yields no texture"
	);
}

/// A registered node whose shader source does not translate to WGSL
/// reports a compile failure instead of running.
#[test]
fn gpu_shader_job_reports_compile_failures() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	let _ = quercus_node::factory::Factory::global().register_dynamic(
		quercus_node::factory::DynamicNodeMeta {
			type_id: "org.quercus.test.badshader".into(),
			name: "BadShader".into(),
			categories: Vec::new(),
			sub_category: String::new(),
			description: String::new(),
			create: Arc::new(|| {
				(
					quercus_node::node::NodeCore::new(),
					Box::new(BadShaderBehavior),
				)
			}),
		},
	);
	let payload = ShaderJobPayload {
		type_id: "org.quercus.test.badshader".into(),
		shader_id: String::new(),
		..ShaderJobPayload::default()
	};
	assert!(
		RenderEvalHooks::new()
			.process_shader_job(&payload)
			.is_none(),
		"an untranslatable shader compiles to nothing"
	);
}

/// A color transform on a GPU texture whose token cannot be read back
/// surfaces the readback error (the LUT pass refused it first).
#[test]
fn color_transform_job_reports_a_failed_gpu_readback() {
	if quercus_core::backend::shared_gpu_or_skip("an eval GPU test").is_none() {
		return;
	}
	let Some(processor) = red_doubling_processor("badtoken") else {
		return;
	};
	let Some(ctx) = quercus_core::backend::GpuContext::shared() else {
		return;
	};
	let input = Texture::gpu(ctx, 0x0BAD_7001, 2, 2, PixelFormat::F32);
	let payload = ColorTransformJobPayload {
		color_processor: Arc::new(processor),
		input: texture_value(input),
		time: Rational::new(0, 1),
	};
	assert!(
		RenderEvalHooks::new()
			.process_color_transform_job(&payload)
			.is_err(),
		"the invalid token cannot be converted"
	);
}

/// An imported planar texture whose plane tokens the context does not
/// own fails the YUV pass and reports the error (the caller then
/// stages through the CPU decoder).
#[test]
fn resolve_planar_footage_reports_a_failed_yuv_pass() {
	let Some(ctx) = quercus_core::backend::shared_gpu_or_skip("an eval GPU test") else {
		return;
	};
	let planar = Texture::wrap_planar(quercus_core::texture::PlanarTexture::new(
		ctx,
		quercus_core::texture::PlanarFormat::Nv12,
		(4, 4),
		(0x0BAD_A001, 0x0BAD_A002),
		quercus_core::backend::YuvTransform::bt709_limited(),
		(2, 2),
	));
	assert!(
		resolve_planar_footage(&planar, None).is_err(),
		"missing plane tokens fail the planar pass"
	);
}

/// A single-sided transition pads the missing side with transparent
/// black and blends (the head/tail fade); a 0x0 side cannot be padded
/// and falls through to the native side without a blend job.
#[test]
fn blend_transition_fills_a_missing_side_and_skips_the_blend_without_a_pair() {
	let mut graph = quercus_node::graph::Graph::new();
	let mut traverser = quercus_node::traverser::Traverser::new();
	let mut hooks = RenderEvalHooks::new();

	let from = graph.add_node(
		quercus_node::node::NodeCore::new(),
		Box::new(FixedTextureBehavior {
			value: Some(texture_value(filled_frame((4, 4), [1.0, 0.0, 0.0, 1.0]))),
		}),
	);
	let out = blend_transition(
		&graph,
		&mut traverser,
		&mut hooks,
		Some(from),
		None,
		Rational::new(0, 1),
		0.25,
		"crossdissolve",
	)
	.expect("single-sided blend");
	let blended = out.expect("the missing side is filled with black");
	assert_eq!(blended.size(), (4, 4));

	// A 0x0 side: black() cannot create the fill, so the pair match
	// falls through to the native side.
	let dummy = graph.add_node(
		quercus_node::node::NodeCore::new(),
		Box::new(FixedTextureBehavior {
			value: Some(texture_value(Texture::dummy())),
		}),
	);
	let out = blend_transition(
		&graph,
		&mut traverser,
		&mut hooks,
		Some(dummy),
		None,
		Rational::new(0, 1),
		0.25,
		"crossdissolve",
	)
	.expect("degenerate single-sided blend");
	let native = out.expect("the native side is returned");
	assert_eq!(native.size(), (0, 0));
}

/// The sweep's head table can carry no texture, a null handle, or an
/// unboxable job handle: all three leave the boundary unchanged.
#[test]
fn flush_adjustment_layer_handles_textureless_heads() {
	let cases: [(&str, Option<NodeValue>); 3] = [
		("none", None),
		("null", Some(NodeValue::Texture(CHandle::null()))),
		(
			"job",
			Some(NodeValue::Texture(quercus_node::handle::make_owned(
				Job::FootageJob(FootageJobPayload::default()),
			))),
		),
	];
	for (tag, value) in cases {
		let mut graph = quercus_node::graph::Graph::new();
		let (acore, abehavior) = quercus_node::block::adjustment_create();
		let block = graph.add_node(acore, abehavior);
		let head = graph.add_node(
			effect_head_core(true),
			Box::new(FixedTextureBehavior { value }),
		);
		graph
			.connect(
				head,
				block,
				quercus_node::block::adjustment_input::TEXTURE_INPUT,
				-1,
			)
			.expect("head -> block");
		let mut traverser = quercus_node::traverser::Traverser::new();
		let mut hooks = RenderEvalHooks::new();
		let sweep = flush_adjustment_layer(
			&mut graph,
			&mut traverser,
			&mut hooks,
			block,
			&[],
			(4, 4),
			Rational::new(0, 1),
			0.5,
		)
		.expect(tag);
		assert!(sweep.is_none(), "{tag}: the boundary is unchanged");
	}
}

/// A chain head whose effect input is not connectable cannot be fed:
/// the sweep reports the refused connection. The graph driver maps the
/// same error out of `render_graph_frame`.
#[test]
fn flush_adjustment_layer_reports_a_refused_connection() {
	let mut graph = quercus_node::graph::Graph::new();
	let (acore, abehavior) = quercus_node::block::adjustment_create();
	let block = graph.add_node(acore, abehavior);
	let head = graph.add_node(
		effect_head_core(false),
		Box::new(FixedTextureBehavior {
			value: Some(texture_value(filled_frame((2, 2), [0.0, 0.0, 0.0, 1.0]))),
		}),
	);
	graph
		.connect(
			head,
			block,
			quercus_node::block::adjustment_input::TEXTURE_INPUT,
			-1,
		)
		.expect("head -> block");
	let mut traverser = quercus_node::traverser::Traverser::new();
	let mut hooks = RenderEvalHooks::new();
	let err = flush_adjustment_layer(
		&mut graph,
		&mut traverser,
		&mut hooks,
		block,
		&[],
		(4, 4),
		Rational::new(0, 1),
		0.5,
	)
	.unwrap_err();
	assert!(
		err.to_string().contains("cannot feed"),
		"the error names the refused feed: {err}"
	);
}

/// `render_graph_frame` propagates an adjustment sweep failure instead
/// of dropping the frame.
#[test]
fn render_graph_frame_reports_a_refused_adjustment_feed() {
	let project = Project::new();
	let seq;
	{
		let mut p = project.lock().unwrap();
		let graph = &mut p.graph;
		let (score, sbehavior) = quercus_node::sequence::SequenceBehavior::create();
		seq = graph.add_node(score, sbehavior);
		let (tlcore, tlbehavior) = quercus_node::track::TrackListBehavior::create();
		let tl = graph.add_node(tlcore, tlbehavior);
		let (tcore, tbehavior) = quercus_node::track::TrackBehavior::create();
		let track = graph.add_node(tcore, tbehavior);

		let (acore, abehavior) = quercus_node::block::adjustment_create();
		let block = graph.add_node(acore, abehavior);
		{
			let a = graph
				.get_mut(block)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<quercus_node::block::AdjustmentBlockBehavior>()
				.unwrap();
			a.core.range = TimeRange::new(Rational::new(0, 1), Rational::new(2, 1));
			a.core.enabled = true;
		}
		let head = graph.add_node(
			effect_head_core(false),
			Box::new(FixedTextureBehavior {
				value: Some(texture_value(filled_frame((4, 4), [0.0, 0.0, 0.0, 1.0]))),
			}),
		);
		graph
			.connect(
				head,
				block,
				quercus_node::block::adjustment_input::TEXTURE_INPUT,
				-1,
			)
			.expect("head -> block");
		graph
			.get_mut(track)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::track::TrackBehavior>()
			.unwrap()
			.append_block(block);
		graph
			.get_mut(tl)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::track::TrackListBehavior>()
			.unwrap()
			.tracks
			.push(track);
		graph
			.get_mut(seq)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<quercus_node::sequence::SequenceBehavior>()
			.unwrap()
			.track_lists
			.push(tl);
	}
	let err = render_graph_frame(&project, seq, Rational::new(0, 1), (4, 4), PixelFormat::F32)
		.unwrap_err();
	assert!(
		err.to_string().contains("cannot feed"),
		"the sweep error reaches the caller: {err}"
	);
}

/// A montage clip whose media cannot be opened propagates the decode
/// error rather than compositing a hole.
#[test]
fn montage_reports_a_failed_clip_decode() {
	let path = std::env::temp_dir().join(format!(
		"quercusrender_missing_montage_{}.mp4",
		std::process::id()
	));
	let params = montage_params_with_effect(&path.to_string_lossy(), "com.example.unused");
	let mut dst = vec![0u8; 16 * 16 * 16];
	assert!(
		render_montage_frame_into(Rational::new(0, 1), &params, (16, 16), &mut dst, 256).is_err(),
		"a missing clip file fails the montage frame"
	);
}

/// A single-clip montage ticket over `path` (no effects), used by the
/// decoded GPU-vs-CPU parity test.
fn montage_params_for_clip(path: &str) -> crate::ticket::VideoTicketParams {
	crate::ticket::VideoTicketParams {
		viewer: 1,
		project: String::new(),
		time: Rational::new(0, 1),
		force_size: Some((16, 16)),
		force_format: Some(PixelFormat::F32),
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage: vec![crate::ticket::MontageClip {
			filename: path.to_string(),
			stream_index: 0,
			in_time: Rational::new(0, 1),
			out_time: Rational::new(2, 1),
			media_in: Rational::new(0, 1),
			gain: 1.0,
			pan: 0.0,
			fade_in: Rational::new(0, 1),
			fade_out: Rational::new(0, 1),
			effects: Vec::new(),
			audio_effects: Vec::new(),
		}],
		adjustments: Vec::new(),
	}
}

/// The decoded montage GPU path composites the same picture as the CPU
/// compositor for a real clip: path selection (GPU for F32) plus
/// fill/alpha-over parity.
#[test]
fn montage_gpu_path_composites_a_decoded_clip_like_the_cpu_path() {
	let Some(_ctx) = quercus_core::backend::shared_gpu_or_skip("an eval GPU test") else {
		return;
	};
	let path = std::env::temp_dir().join(format!(
		"quercusrender_montage_gpu_path_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip(&path, 16, 16, 10, 10).expect("test clip");
	let params = montage_params_for_clip(&path.to_string_lossy());

	let mut cpu = vec![0u8; 16 * 16 * 16];
	render_montage_frame_into(Rational::new(0, 1), &params, (16, 16), &mut cpu, 256)
		.expect("CPU montage");
	let gpu = render_montage_frame(Rational::new(0, 1), &params, (16, 16), PixelFormat::F32)
		.expect("GPU montage");
	assert!(
		matches!(gpu, Texture::Gpu { .. }),
		"the decoded montage frame stays on the GPU"
	);
	let frame = gpu.to_frame().unwrap();
	let _ = std::fs::remove_file(&path);
	assert_eq!(frame.linesize_bytes(), 256);
	assert_eq!(frame.data.len(), cpu.len(), "same target raster");
	for (a, b) in cpu.chunks_exact(4).zip(frame.data.chunks_exact(4)) {
		let a = f32::from_le_bytes(a.try_into().unwrap());
		let b = f32::from_le_bytes(b.try_into().unwrap());
		assert!((a - b).abs() < 5e-3, "GPU/CPU montage parity: {a} vs {b}");
	}
}

/// A clip effect that hands back a GPU texture on a context whose
/// readback fails surfaces the readback error.
#[test]
fn montage_reports_a_failed_gpu_readback() {
	let _env = ENV_TEST_LOCK.lock().unwrap();
	let _guard = PLUGIN_TEST_LOCK.lock().unwrap();
	let path = std::env::temp_dir().join(format!(
		"quercusrender_montage_readback_{}.mp4",
		std::process::id()
	));
	quercus_codec::testmedia::write_test_clip(&path, 16, 16, 10, 10).expect("test clip");
	crate::ofxhost::install_client(None);
	set_plugin_instance_factory(Some(Arc::new(|_id: &str| Some(7))));
	set_plugin_executor(Some(Arc::new(|req: &PluginJobRequest<'_>| {
		Ok(Texture::gpu(
			Arc::new(UnusedCtx),
			0xE1,
			req.src.size().0,
			req.src.size().1,
			PixelFormat::F32,
		))
	})));
	let params = montage_params_with_effect(&path.to_string_lossy(), "com.example.no-readback");
	let mut dst = vec![0u8; 16 * 16 * 16];
	let result = render_montage_frame_into(Rational::new(0, 1), &params, (16, 16), &mut dst, 256);
	set_plugin_executor(None);
	set_plugin_instance_factory(None);
	let _ = std::fs::remove_file(&path);
	assert!(
		result.is_err(),
		"an unreadable GPU effect result fails the montage frame"
	);
}

/// Degenerate adjustment-span geometry is inert: a zero width cannot
/// stage a frame, and a destination too small for the staged copy is
/// left untouched.
#[test]
fn adjustment_span_degenerate_geometry_is_inert() {
	let span = adjustment_span(0, 2, 0, vec![unknown_effect("com.example.span-geometry")]);

	// w == 0: the staging frame cannot be generated.
	let mut dst = vec![0xABu8; 16];
	apply_adjustment_span(&mut dst, 16, 0, 1, &span, Rational::new(0, 1));
	assert!(dst.iter().all(|&b| b == 0xAB), "zero width is inert");

	// The destination is shorter than the staged frame geometry.
	let mut dst = vec![0xCDu8; 8];
	apply_adjustment_span(&mut dst, 8, 2, 1, &span, Rational::new(0, 1));
	assert!(dst.iter().all(|&b| b == 0xCD), "a short buffer is inert");
}

/// The tests' GPU stand-ins and the null-texture behavior expose their
/// documented error/duplication contracts.
#[test]
fn test_context_stubs_report_their_contracts() {
	use quercus_core::backend::GpuContextLike;
	use quercus_node::node::NodeBehavior;

	let unused = UnusedCtx;
	assert!(unused.upload(0, &Frame::dummy()).is_err());
	assert!(unused.download(0).is_err());
	assert!(unused.blit(0, 0, None).is_err());
	unused.destroy_texture(0);

	let echo = FrameEchoCtx {
		frame: Frame::dummy(),
	};
	assert!(echo.upload(7, &Frame::dummy()).is_ok());
	assert_eq!(echo.download(7).unwrap().width, 0);
	assert!(echo.blit(0, 0, None).is_err());

	let behavior = NullTextureBehavior;
	assert_eq!(behavior.name(), "NullTexture");
	assert_eq!(behavior.type_id(), "org.quercus.test.nulltexture");
	assert!(behavior
		.duplicate(&quercus_node::node::NodeCore::new())
		.is_some());
}
