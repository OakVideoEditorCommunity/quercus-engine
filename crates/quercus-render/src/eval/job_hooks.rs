use super::*;

impl RenderEvalHooks {
	/// A hook set with every optional hook unset (`use_cache` off).
	pub fn new() -> Self {
		Self {
			use_cache: false,
			ticket: None,
			frame_size: None,
			sequence_size: None,
			layer_progress: None,
		}
	}

	/// C++ process_color_transform: apply the job's OCIO processor to the
	/// input texture. A CPU frame converts in place (the real OCIO
	/// `convert_frame`); a GPU input gets the same transform baked into a
	/// 3D LUT and applied by the GPU LUT pass (M2: color management is
	/// never skipped), with a readback+convert+re-upload fallback for
	/// exotic processors. An invalid processor passes the input through
	/// unchanged (C++ creates processors non-fatally); a non-texture input
	/// is `Error::Invalid`.
	pub(crate) fn process_color_transform_job(
		&mut self,
		payload: &ColorTransformJobPayload,
	) -> Result<Texture> {
		let NodeValue::Texture(handle) = &payload.input else {
			return Err(Error::Invalid);
		};
		if handle.ctx.is_null() {
			return Err(Error::Invalid);
		}
		let tex = (unsafe { quercus_node::handle::get_checked::<Texture>(handle) })
			.cloned()
			.ok_or(Error::Invalid)?;
		if !payload.color_processor.is_valid() {
			// No valid processor (no default config, LUT load failure):
			// pass the input through, mirroring the C++ non-fatal
			// processor creation.
			return Ok(tex);
		}
		let mut tex = tex;
		if let Texture::Cpu(frame) = &mut tex {
			// The CPU leg converts in place (real OCIO `convert_frame`).
			payload.color_processor.convert_frame(frame)?;
			return Ok(tex);
		}
		// GPU leg: bake the processor into a 3D LUT and run the GPU color
		// pass (no readback). Fall back to an explicit readback + CPU
		// conversion + re-upload when the processor cannot be baked or the
		// texture's context is not a real GPU context.
		let (token, ctx, width, height) = match &tex {
			Texture::Gpu {
				token,
				ctx,
				width,
				height,
				..
			} => (*token, ctx.clone(), *width, *height),
			// Imported planar frames never reach node processing (the
			// footage path resolves them first); pass through defensively.
			Texture::Cpu(_) | Texture::Planar(_) => return Ok(tex),
		};
		let concrete = ctx
			.as_any()
			.and_then(|a| a.downcast_ref::<quercus_core::backend::GpuContext>());
		if let Some(concrete) = concrete {
			if let Some(lut) = color_transform_lut(&payload.color_processor) {
				if let Ok(dst) =
					concrete.apply_color_lut(token, &payload.color_processor.cache_id(), &lut)
				{
					return Ok(Texture::gpu(ctx, dst, width, height, PixelFormat::F32));
				}
			}
			let mut frame = tex.to_frame()?;
			payload.color_processor.convert_frame(&mut frame)?;
			let dst = concrete.create_texture(frame.width, frame.height)?;
			concrete.upload(dst, &frame)?;
			return Ok(Texture::gpu(
				ctx,
				dst,
				frame.width,
				frame.height,
				PixelFormat::F32,
			));
		}
		let mut frame = tex.to_frame()?;
		payload.color_processor.convert_frame(&mut frame)?;
		Ok(Texture::wrap_frame(frame))
	}

	/// C++ process_frame_generation: fill the destination with a generated
	/// F32 frame (transparent black for now).
	///
	/// Only the `generation_fills_cpu_texture` unit test drives this today;
	/// the live eval path uses the shader/graph hooks instead.
	#[allow(dead_code)]
	pub(crate) fn process_frame_generation(
		&mut self,
		destination: &mut Texture,
		time: Rational,
	) -> Result<()> {
		let Texture::Cpu(frame) = destination else {
			return Err(Error::Failed(
				"frame generation on GPU deferred: CPU path only this pass".into(),
			));
		};
		let generated = generate_frame(time, (frame.width, frame.height), frame.format)?;
		frame.data = generated.data;
		frame.timestamp = time;
		Ok(())
	}

	/// C++ process_plugin_job: dispatch through the single OFX host
	/// process when one is installed (M3), else through the in-process
	/// plugin executor (the quercusplugin render driver; dependency
	/// inversion). A missing host/executor or a failed render yields a
	/// purple failure frame instead of aborting the graph, matching
	/// pluginjob.cpp's fallback.
	///
	/// `src` is the texture the plugin renders against (the caller picks
	/// the job's effect input, else the first clip texture, else a dummy
	/// 1×1); `spec` must be a [`JobSpec::Plugin`] — any other variant
	/// returns `Error::Invalid`. The job's `size` comes from `src`.
	/// Only a non-plugin spec is an `Err`; plugin render failures are
	/// logged and reported as [`purple_frame`] so the graph continues.
	pub(crate) fn process_plugin_job(&mut self, src: Texture, spec: &JobSpec) -> Result<Texture> {
		let JobSpec::Plugin {
			instance,
			type_id,
			time,
			effect_input_id,
			inputs,
			values,
			animations: _,
		} = spec
		else {
			return Err(Error::Invalid);
		};
		let size = src.size();
		if let Some(host) = crate::ofxhost::client() {
			return match host.submit(spec, &src) {
				Ok(frame) => Ok(Texture::wrap_frame(frame)),
				Err(err) => {
					eprintln!(
						"OFX host job {type_id} (instance {instance}) at t={time}s failed: {err:#}"
					);
					Ok(purple_frame(Rational::from_double(*time), size))
				}
			};
		}
		let Some(executor) = plugin_executor() else {
			return Ok(purple_frame(Rational::from_double(*time), size));
		};
		let _ = (instance, type_id, effect_input_id, inputs, values);
		match executor(&PluginJobRequest { spec, src }) {
			Ok(texture) => Ok(texture),
			Err(err) => {
				eprintln!("plugin instance {instance} render at t={time}s failed: {err:#}");
				Ok(purple_frame(Rational::from_double(*time), size))
			}
		}
	}

	/// C++ process_video_cache_job: read the frame the cache wrote at
	/// `payload.path` through the disk frame-cache container
	/// ([`crate::frameio`]). A missing or unreadable file is an `Err` —
	/// the caller then substitutes the job's fallback value. The
	/// payload's request time is already spelled into `path` (the cache
	/// writes one file per frame), so the read itself ignores it.
	fn process_cache_job(&mut self, payload: &CacheJobPayload) -> Result<Texture> {
		let frame = crate::frameio::load_cache_frame(&payload.path)?;
		Ok(Texture::wrap_frame(frame))
	}

	/// Resolve one texture-channel value in place (the C++ JobEngine's
	/// per-value walk): a boxed [`Job`] recurses into its own inputs and
	/// runs, then the box is replaced by the resulting texture; a genuine
	/// texture — or any non-texture value — passes through untouched.
	/// `depth` bounds the nesting; `in_flight` holds the job boxes on the
	/// current walk so a job reachable from itself stops instead of
	/// recursing forever (the C++ `resolved_texture_cache_`
	/// de-duplication, kept as a path set so the same handle reached from
	/// two rows still resolves per row).
	pub(crate) fn resolve_value(
		&mut self,
		value: &mut NodeValue,
		depth: usize,
		in_flight: &mut HashSet<usize>,
	) {
		/// Job-nesting recursion ceiling (defensive; real graphs nest a
		/// generator job inside a merge job and stop there).
		const MAX_JOB_DEPTH: usize = 64;
		if depth >= MAX_JOB_DEPTH {
			return;
		}
		let NodeValue::Texture(handle) = value else {
			return;
		};
		if handle.ctx.is_null() {
			return;
		}
		let key = handle.ctx as usize;
		if !in_flight.insert(key) {
			return;
		}
		let job = (unsafe { quercus_node::jobs::job_ref(handle) }).cloned();
		if let Some(job) = job {
			if let Some(resolved) = self.process_job(&job, depth + 1, in_flight) {
				*value = resolved;
			}
		}
		in_flight.remove(&key);
	}

	/// Run one resolved [`Job`] (the C++ `process_*` virtual, dispatched on
	/// the payload type). `None` when the job produced no replacement
	/// value — the row then keeps its box.
	fn process_job(
		&mut self,
		job: &Job,
		depth: usize,
		in_flight: &mut HashSet<usize>,
	) -> Option<NodeValue> {
		match job {
			Job::FootageJob(payload) => self.process_footage_job_value(payload),
			Job::ShaderJob(payload) => {
				Some(self.process_shader_job_value(payload, depth, in_flight))
			}
			Job::PluginJob(payload) => self.process_plugin_job_value(payload, depth, in_flight),
			Job::ColorTransformJob(payload) => {
				Some(self.process_color_transform_job_value(payload, depth, in_flight))
			}
			Job::CacheJob(payload) => Some(self.process_cache_job_value(payload, depth, in_flight)),
		}
	}

	/// Resolve one footage job (C++ FootageJob processing in
	/// jobmanager.cpp): decode the frame at the job's request time and
	/// replace the box with the resulting texture. `None` on a decode
	/// failure — the row keeps its box, so the failure stays visible and
	/// is retried rather than cached as a hole.
	pub(crate) fn process_footage_job_value(
		&mut self,
		payload: &FootageJobPayload,
	) -> Option<NodeValue> {
		let size = self.frame_size.unwrap_or((0, 0));
		match render_footage_frame(
			&payload.filename,
			payload.stream_index,
			payload.time,
			size,
			PixelFormat::F32,
		) {
			Ok(texture) => Some(texture_value(texture)),
			Err(err) => {
				eprintln!("footage job decode failed: {err:#}");
				None
			}
		}
	}

	/// Resolve one shader job (C++ ShaderJob processing in jobmanager.cpp):
	/// recurse into the param row's job boxes (the generator layer of an
	/// `mrg` chain), execute the pass, and replace the box with the result
	/// texture. A failed or un-runnable job falls back to the effect input
	/// texture from the now-resolved param row (a pass-through — C++ leaves
	/// the failed shader's output as its input); a row without the effect
	/// input resolves to `NodeValue::None`.
	///
	/// `payload` is the shader job whose `params` row is resolved in place;
	/// `depth`/`in_flight` are the recursion state, and each param value is
	/// resolved at the same `depth` (nested payload recursion happens inside
	/// [`Self::resolve_value`]/[`Self::process_shader_job`]). Always returns
	/// a value: the rendered texture, the effect-input value, or
	/// `NodeValue::None`.
	fn process_shader_job_value(
		&mut self,
		payload: &ShaderJobPayload,
		depth: usize,
		in_flight: &mut HashSet<usize>,
	) -> NodeValue {
		let mut payload = payload.clone();
		for value in payload.params.values_mut() {
			self.resolve_value(value, depth, in_flight);
		}
		match self.process_shader_job(&payload) {
			Some(texture) => texture_value(texture),
			None => payload
				.params
				.get(&payload.effect_input)
				.cloned()
				.unwrap_or(NodeValue::None),
		}
	}

	/// Resolve one plugin job (C++ JobEnginePlugin processing in
	/// jobmanager.cpp): recurse into the payload's tagged values, split
	/// them into clip input textures and scalar param overrides, then
	/// dispatch the render through the installed executor. A failed render
	/// leaves the box in the row (the executor has already reported it).
	pub(crate) fn process_plugin_job_value(
		&mut self,
		payload: &PluginJobPayload,
		depth: usize,
		in_flight: &mut HashSet<usize>,
	) -> Option<NodeValue> {
		let mut payload = payload.clone();
		for value in payload.values.values_mut() {
			self.resolve_value(value, depth, in_flight);
		}

		let mut inputs: Vec<(String, Texture)> = Vec::new();
		let mut values: Vec<(String, NodeValue)> = Vec::new();
		for (key, v) in payload.values.iter() {
			match v {
				NodeValue::Texture(h) if !h.ctx.is_null() => {
					match unsafe { quercus_node::handle::get_checked::<Texture>(h) }.cloned() {
						Some(texture) => inputs.push((key.clone(), texture)),
						None => eprintln!("plugin job input '{key}' is not a texture box"),
					}
				}
				NodeValue::Texture(_) | NodeValue::None => {}
				other => values.push((key.clone(), other.clone())),
			}
		}

		// Fallback order mirrors pluginrenderer.cpp's effect input
		// resolution: the declared effect input, else the first
		// available clip texture.
		let effect_src = if payload.effect_input_id.is_empty() {
			None
		} else {
			inputs
				.iter()
				.find(|(key, _)| key == &payload.effect_input_id)
				.map(|(_, t)| t.clone())
		};
		let src = effect_src
			.or_else(|| inputs.first().map(|(_, t)| t.clone()))
			.unwrap_or_else(Texture::dummy);

		let spec = JobSpec::Plugin {
			instance: payload.instance.0,
			type_id: payload.type_id.clone(),
			time: payload.time.to_f64(),
			effect_input_id: if payload.effect_input_id.is_empty() {
				None
			} else {
				Some(payload.effect_input_id.clone())
			},
			inputs,
			values,
			animations: payload.animations.clone(),
		};
		match self.process_plugin_job(src, &spec) {
			Ok(texture) => Some(texture_value(texture)),
			Err(err) => {
				eprintln!("plugin job resolve failed: {err:#}");
				None
			}
		}
	}

	/// Resolve one color transform job (C++ ColorTransformJob processing in
	/// jobmanager.cpp): recurse into the input value, apply the processor,
	/// and replace the box with the result. A failure falls back to the
	/// job's resolved input texture (a pass-through — the C++ renderer
	/// leaves the failed transform's output as its input).
	///
	/// `payload` is the color-transform job (input value plus processor);
	/// `depth`/`in_flight` are forwarded to the recursion walk that
	/// resolves the input first. Always returns a value: the transformed
	/// texture, or the resolved input when the transform fails (logged).
	pub(crate) fn process_color_transform_job_value(
		&mut self,
		payload: &ColorTransformJobPayload,
		depth: usize,
		in_flight: &mut HashSet<usize>,
	) -> NodeValue {
		let mut payload = payload.clone();
		self.resolve_value(&mut payload.input, depth, in_flight);
		match self.process_color_transform_job(&payload) {
			Ok(texture) => texture_value(texture),
			Err(err) => {
				eprintln!("color transform job failed: {err:#}");
				payload.input.clone()
			}
		}
	}

	/// Resolve one cache job (C++ CacheJob processing in jobmanager.cpp):
	/// recurse into the fallback value first, then read the frame the cache
	/// wrote at the job's path. A missing or unreadable file substitutes
	/// the fallback — a real texture by then, not another job box — and is
	/// logged once per path (a cache miss repeats every frame).
	///
	/// `payload` is the cache job (path plus fallback value); `depth` and
	/// `in_flight` are the recursion state shared with
	/// [`Self::resolve_value`], which resolves the fallback at the same
	/// `depth` and uses `in_flight` to stop a job reachable from itself.
	/// Always returns a value: the cached frame on success, else the
	/// resolved fallback.
	fn process_cache_job_value(
		&mut self,
		payload: &CacheJobPayload,
		depth: usize,
		in_flight: &mut HashSet<usize>,
	) -> NodeValue {
		let mut payload = payload.clone();
		self.resolve_value(&mut payload.fallback, depth, in_flight);
		match self.process_cache_job(&payload) {
			Ok(texture) => texture_value(texture),
			Err(err) => {
				let key = format!("cache:{}", payload.path);
				if unsupported_warned().insert(key) {
					eprintln!(
						"cache job \"{}\" failed, using the cache node's input: {err:#}",
						payload.path
					);
				}
				(*payload.fallback).clone()
			}
		}
	}

	/// Execute one shader payload (C++ process_shader run by the render
	/// worker): compile the emitting behavior's fragment shader on the
	/// shared GPU context and run the requested iterations. **Every**
	/// texture-typed param is bound by its input id (C++ binds all
	/// sampler inputs — merge's base/blend, the keyers' garbage/core
	/// mattes, opacity's texture modulation); a param boxing a nested
	/// shader payload resolves recursively first (the C++
	/// AcceleratedJob chain — generator-over-base `mrg` jobs). CPU
	/// frames upload into scratch textures; the pass size comes from
	/// the effect input's texture (else the first bound texture, else
	/// the hook's frame size, else 1x1). `None` when the job cannot run
	/// (no GPU context, unknown node type, missing shader, or a
	/// compile/upload/run failure) — the caller then falls back to the
	/// effect input texture.
	pub(crate) fn process_shader_job(&self, payload: &ShaderJobPayload) -> Option<Texture> {
		self.process_shader_job_depth(payload, 0)
	}

	/// [`Self::process_shader_job`] with a recursion guard for nested
	/// payloads (generator-over-base chains nest at most 2 deep).
	///
	/// `payload` is the shader job to execute — its `params` row holds
	/// texture boxes, scalar params and nested shader payloads; `depth` is
	/// the current nesting level, 0 for the top-level job. A texture param
	/// boxing another shader payload is resolved by a recursive call with
	/// `depth + 1`; once `depth` reaches 8 (defensive ceiling; real chains
	/// nest at most 2) the nested box is treated as unresolved and that
	/// sampler is left unbound. Returns `None` when the job cannot run, for
	/// the reasons listed on [`Self::process_shader_job`].
	pub(crate) fn process_shader_job_depth(
		&self,
		payload: &ShaderJobPayload,
		depth: u32,
	) -> Option<Texture> {
		/// Nested-payload recursion ceiling (defensive; real graphs nest
		/// a generator job inside a merge job and stop there).
		const MAX_JOB_DEPTH: u32 = 8;

		// One log line per shader per process instead of one per frame.
		let warn = |reason: &str| {
			let key = format!("shader:{}:{}", payload.type_id, payload.shader_id);
			if unsupported_warned().insert(key) {
				eprintln!(
					"shader job \"{}\" (shader \"{}\") failed: {reason}",
					payload.type_id, payload.shader_id
				);
			}
		};

		let Some(ctx) = quercus_core::backend::GpuContext::shared() else {
			warn("no GPU context");
			return None;
		};

		// The emitting node behavior: the type id selects the fragment
		// source (C++ `node->get_shader_code(shader_id)`).
		let Some((_, behavior)) =
			quercus_node::factory::Factory::global().create_any(&payload.type_id)
		else {
			warn("unknown node type");
			return None;
		};

		// OCIO-based nodes splice the auto-generated OCIO function into
		// their `%1` marker (C++ `GetShaderCode({shader_id, stub})`, the
		// stub built in colormanagement.cpp `GetColorContext`). A stub
		// that cannot be generated — no default config, or a LUT
		// processor with no upload path — falls back to the effect input
		// pass-through.
		let ocio_entry = OCIO_SHADER_STUBS
			.iter()
			.find(|(id, ..)| *id == payload.type_id)
			.copied();
		let grading_entry = OCIO_GRADING_STUBS
			.iter()
			.find(|(id, _)| *id == payload.type_id)
			.copied();
		let glsl = match (grading_entry, ocio_entry) {
			(Some((_, style)), _) => {
				let Some(stub) = quercus_core::color::grading_primary_function_shader(style) else {
					return None;
				};
				match behavior.shader_code(&stub) {
					Some(glsl) => glsl,
					None => {
						warn("shader not found");
						return None;
					}
				}
			}
			(None, Some((_, fn_name, from, to))) => {
				let Some(stub) = quercus_core::color::ocio_function_shader(fn_name, from, to)
				else {
					return None;
				};
				match behavior.shader_code(&stub) {
					Some(glsl) => glsl,
					None => {
						warn("shader not found");
						return None;
					}
				}
			}
			(None, None) => match behavior.shader_code(&payload.shader_id) {
				Some(glsl) => glsl,
				None => {
					warn("shader not found");
					return None;
				}
			},
		};

		// Pipeline cache key: the type id plus the shader-variant id (the
		// OCIO stub text folds in too, so a config change recompiles
		// instead of reusing a stale variant).
		let spliced_ocio = grading_entry.is_some() || ocio_entry.is_some();
		let key = if spliced_ocio {
			let mut h = std::collections::hash_map::DefaultHasher::new();
			std::hash::Hash::hash(&glsl, &mut h);
			format!(
				"{}:{}:ocio:{}",
				payload.type_id,
				payload.shader_id,
				std::hash::Hasher::finish(&h)
			)
		} else {
			format!("{}:{}", payload.type_id, payload.shader_id)
		};
		let compiled = match compile_effect(&ctx, &key, &glsl, ctx.is_filterable()) {
			Ok(effect) => effect,
			Err(err) => {
				warn(&format!("compile failed: {err:#}"));
				return None;
			}
		};

		// Bind every texture-typed param by name: genuine texture boxes
		// bind directly (CPU frames upload into scratch first); nested
		// shader payloads (the generator layer of an `mrg` job) resolve
		// recursively. `scratch` holds the upload tokens created here;
		// `keepalive` holds the cloned `Texture`s — a `Texture::Gpu`
		// clone destroys its token on drop, so the clones must outlive
		// the pass. Both are released when the job finishes (input-token
		// destruction at job end matches the historical semantics).
		let mut inputs: Vec<(String, u64)> = Vec::new();
		let mut scratch: Vec<u64> = Vec::new();
		let mut keepalive: Vec<Texture> = Vec::new();
		let mut size: Option<(i32, i32)> = None;
		let bind = |key: &str,
		            value: &NodeValue,
		            inputs: &mut Vec<(String, u64)>,
		            scratch: &mut Vec<u64>,
		            keepalive: &mut Vec<Texture>,
		            size: &mut Option<(i32, i32)>|
		 -> Option<()> {
			let NodeValue::Texture(handle) = value else {
				return None;
			};
			if handle.ctx.is_null() {
				return None;
			}
			let tex = (unsafe { quercus_node::handle::get_checked::<Texture>(handle) })
				.cloned()
				.or_else(|| {
					if depth >= MAX_JOB_DEPTH {
						return None;
					}
					let nested = (unsafe { quercus_node::jobs::shader_job(handle) }).cloned()?;
					self.process_shader_job_depth(&nested, depth + 1)
				});
			let tex = tex?;
			let (token, tex_size) = match &tex {
				Texture::Gpu {
					token,
					width,
					height,
					..
				} => (*token, (*width, *height)),
				Texture::Cpu(frame) => {
					let token = match ctx.create_texture(frame.width, frame.height) {
						Ok(t) => t,
						Err(err) => {
							warn(&format!("input texture: {err:#}"));
							return None;
						}
					};
					if let Err(err) = ctx.upload(token, frame) {
						ctx.destroy_texture(token);
						warn(&format!("input upload failed: {err:#}"));
						return None;
					}
					scratch.push(token);
					(token, (frame.width, frame.height))
				}
				// Resolved by the footage path; never a shader input.
				Texture::Planar(_) => {
					warn("planar texture reached a shader job input (unresolved)");
					return None;
				}
			};
			// The pass size follows the effect input's texture (C++ the
			// job's video params = the main input size); any other bound
			// texture sets it only when no effect input was seen.
			if size.is_none() || key == payload.effect_input {
				*size = Some(tex_size);
			}
			inputs.push((key.to_string(), token));
			keepalive.push(tex);
			Some(())
		};

		// The effect input binds first: `run_effect` falls back to
		// `inputs.first()` for the shader's first declared sampler.
		let effect_value = payload.params.get(&payload.effect_input).cloned();
		if let Some(value) = &effect_value {
			bind(
				&payload.effect_input,
				value,
				&mut inputs,
				&mut scratch,
				&mut keepalive,
				&mut size,
			);
		}
		for (key, value) in &payload.params {
			if key == &payload.effect_input {
				continue;
			}
			bind(
				key,
				value,
				&mut inputs,
				&mut scratch,
				&mut keepalive,
				&mut size,
			);
		}
		// Generators bind no texture: render at the requested frame size
		// (the graph driver sets it to the sequence size); 1x1 only when
		// nobody knows better.
		let size = size.or(self.frame_size).unwrap_or((1, 1));

		// `resolution_in` anchors to the sequence square resolution, not
		// the render target (C++ inserts the NodeGlobals square
		// resolution into the job at build time): the node params it
		// denormalizes (shape size/pos, transform offsets, corner pin
		// points, drop shadow distance) are all sequence-pixel values,
		// so a proxy-size playback render must resolve them against the
		// same resolution as a paused full-res frame, or the effect
		// visibly changes size whenever the transport stops. Pre-filling
		// the row wins over `run_effect`'s frame-size auto-fill; a node
		// that inserted its own `resolution_in` keeps it.
		//
		// `progress_in` is the same pre-fill for the adjustment-layer
		// sweep: a shader declaring the uniform receives the layer
		// progress the graph driver recorded for this evaluation. The
		// row is only cloned when something actually needs inserting.
		let mut anchored_row;
		let needs_resolution = self.sequence_size.is_some()
			&& !payload.params.contains_key("resolution_in")
			&& compiled
				.translated
				.uniforms
				.iter()
				.any(|u| u.name == "resolution_in");
		let needs_progress = self.layer_progress.is_some()
			&& !payload.params.contains_key("progress_in")
			&& compiled
				.translated
				.uniforms
				.iter()
				.any(|u| u.name == "progress_in");
		let params = if needs_resolution || needs_progress {
			anchored_row = payload.params.clone();
			if needs_resolution {
				let (w, h) = self.sequence_size.unwrap();
				anchored_row.insert(
					"resolution_in".to_string(),
					NodeValue::Vec2([w as f64, h as f64]),
				);
			}
			if needs_progress {
				anchored_row.insert(
					"progress_in".to_string(),
					NodeValue::Float(self.layer_progress.unwrap()),
				);
			}
			&anchored_row
		} else {
			&payload.params
		};

		let dst = match ctx.create_texture(size.0.max(1), size.1.max(1)) {
			Ok(t) => t,
			Err(err) => {
				for t in &scratch {
					ctx.destroy_texture(*t);
				}
				warn(&format!("output texture: {err:#}"));
				return None;
			}
		};

		let result = run_effect(
			&ctx,
			&compiled,
			params,
			&inputs,
			dst,
			size,
			payload.iterations.max(1) as u32,
			if payload.iterative_input.is_empty() {
				None
			} else {
				Some(payload.iterative_input.as_str())
			},
		);
		for t in &scratch {
			ctx.destroy_texture(*t);
		}
		match result {
			Ok(()) => Some(Texture::gpu(
				ctx.clone(),
				dst,
				size.0.max(1),
				size.1.max(1),
				PixelFormat::F32,
			)),
			Err(err) => {
				ctx.destroy_texture(dst);
				warn(&format!("run failed: {err:#}"));
				None
			}
		}
	}
}
