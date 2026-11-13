use std::time::Instant;

use quercus_core::{PixelFormat, Rational};
use quercus_render::ticket::{MontageClip, MontageEffect, VideoTicketParams};

fn params(media: &str, width: i32, height: i32, cpu_effect: bool) -> VideoTicketParams {
	VideoTicketParams {
		viewer: 1,
		project: String::new(),
		time: Rational::new(0, 1),
		force_size: Some((width, height)),
		force_format: Some(PixelFormat::F32),
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage: vec![MontageClip {
			filename: media.to_string(),
			stream_index: 0,
			in_time: Rational::new(0, 1),
			out_time: Rational::new(600, 25),
			media_in: Rational::new(0, 1),
			gain: 1.0,
			pan: 0.0,
			fade_in: Rational::new(0, 1),
			fade_out: Rational::new(0, 1),
			effects: if cpu_effect {
				vec![MontageEffect {
					type_id: "example.CpuOnlyEffect".into(),
					enabled: true,
					effect_input_id: None,
					params: Vec::new(),
				}]
			} else {
				Vec::new()
			},
			audio_effects: Vec::new(),
		}],
		adjustments: Vec::new(),
	}
}

fn main() {
	let mut args = std::env::args().skip(1);
	let media = args
		.next()
		.unwrap_or_else(|| "/home/sunyu/Videos/20260404双创宣传片素材拍摄/C0024.MP4".into());
	let workers: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(8);
	let frames: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(4);
	let width: i32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(1920);
	let height: i32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(1080);
	let _ui_device = std::env::var_os("QUERCUS_STRESS_UI")
		.and_then(|_| quercus_core::backend::GpuContext::create(
			quercus_core::backend::BackendKind::Vulkan,
		));
	let pipeline = std::env::var_os("QUERCUS_STRESS_PIPELINE")
		.map(|_| quercus_render::pipeline::PipelineBackend::new().expect("pipeline start"));
	let started = Instant::now();
	let mut tasks = Vec::new();
	for worker in 0..workers {
		let media = media.clone();
		tasks.push(std::thread::spawn(move || {
			let template = params(&media, width, height, worker % 2 == 1);
			for frame in 0..frames {
				let time = Rational::new(frame as i64, 25);
				let mut params = template.clone();
				params.time = time;
				let texture = quercus_render::eval::render_produced_frame(time, &params)?;
				let readback = texture.to_frame();
				if let Err(error) = readback {
					eprintln!("[stress] worker={worker} frame={frame} readback failed: {error}");
					return Err(error);
				}
				eprintln!("[stress] worker={worker} frame={frame} done");
			}
			Ok::<(), quercus_render::error::Error>(())
		}));
	}
	for task in tasks {
		if let Err(error) = task.join().expect("stress worker panicked") {
			eprintln!("[stress] worker failed: {error}");
		}
	}
	eprintln!(
		"[stress] wall={:.2}s imports={} transfers={}",
		started.elapsed().as_secs_f64(),
		quercus_codec::gpuinterop::HW_IMPORTS.load(std::sync::atomic::Ordering::Relaxed),
		quercus_codec::hwdecode::HW_TRANSFERS.load(std::sync::atomic::Ordering::Relaxed),
	);
	if let Some(pipeline) = pipeline {
		quercus_render::worker::JobDispatch::shutdown(&*pipeline);
	}
}
