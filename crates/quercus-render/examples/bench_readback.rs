// Quercus Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Quercus Team
//
// CLI playback consumer for the engine's GPU -> CPU boundary.
//
// Renders sequential montage frames from one media file, times the GPU
// render and the synchronous readback separately, and reports the
// distributions. Used while tuning the async readback pipeline.

use std::time::{Duration, Instant};

use quercus_core::texture::Texture;
use quercus_core::{PixelFormat, Rational};
use quercus_render::ticket::{MontageClip, VideoTicketParams};

fn params(path: &str, width: i32, height: i32) -> VideoTicketParams {
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
			filename: path.to_string(),
			stream_index: 0,
			in_time: Rational::new(0, 1),
			out_time: Rational::new(600, 25),
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

fn percentile(sorted: &[Duration], p: f64) -> Duration {
	if sorted.is_empty() {
		return Duration::ZERO;
	}
	let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
	sorted[idx]
}

fn report(name: &str, mut samples: Vec<Duration>) {
	samples.sort();
	let total: Duration = samples.iter().sum();
	let avg = total / samples.len().max(1) as u32;
	eprintln!(
		"{name:<12} avg={:>8.2}ms p50={:>8.2}ms p95={:>8.2}ms max={:>8.2}ms",
		avg.as_secs_f64() * 1000.0,
		percentile(&samples, 0.5).as_secs_f64() * 1000.0,
		percentile(&samples, 0.95).as_secs_f64() * 1000.0,
		samples.last().copied().unwrap_or_default().as_secs_f64() * 1000.0,
	);
}

fn main() {
	let mut args = std::env::args().skip(1);
	let media = args
		.next()
		.unwrap_or_else(|| "/home/sunyu/Videos/20260404双创宣传片素材拍摄/C0024.MP4".into());
	let frames: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(30);
	let width: i32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(1920);
	let height: i32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(1080);

	if quercus_core::backend::GpuContext::shared().is_none() {
		eprintln!("SKIP: no GPU context");
		return;
	}
	let template = params(&media, width, height);
	let mut render_times = Vec::with_capacity(frames);
	let mut readback_times = Vec::with_capacity(frames);
	let mut gpu_frames = 0usize;
	let wall = Instant::now();
	let mut first_frame = None;
	for n in 0..frames {
		let mut p = template.clone();
		p.time = Rational::new(n as i64, 25);
		let started = Instant::now();
		let texture = quercus_render::eval::render_produced_frame(p.time, &p)
			.expect("montage render");
		render_times.push(started.elapsed());
		if matches!(texture, Texture::Gpu { .. }) {
			gpu_frames += 1;
		}
		let started = Instant::now();
		let frame = texture.to_frame().expect("readback");
		readback_times.push(started.elapsed());
		if first_frame.is_none() {
			first_frame = Some(frame);
		}
	}
	if let Some(frame) = first_frame {
		eprintln!(
			"frame0 {}x{} {} bytes (linesize {})",
			frame.width,
			frame.height,
			frame.data.len(),
			frame.linesize_bytes()
		);
	}
	let elapsed = wall.elapsed();
	eprintln!(
		"frames={frames} gpu={gpu_frames} wall={:.2}s fps={:.1} imports={} transfers={}",
		elapsed.as_secs_f64(),
		frames as f64 / elapsed.as_secs_f64(),
		quercus_codec::gpuinterop::HW_IMPORTS.load(std::sync::atomic::Ordering::Relaxed),
		quercus_codec::hwdecode::HW_TRANSFERS.load(std::sync::atomic::Ordering::Relaxed),
	);
	report("render", render_times);
	report("readback", readback_times);
}
