//! Unit coverage for the safe parts of the import module. The unsafe
//! platform bodies are deliberately **not** exercised here: a real
//! import needs a hardware decoder surface (a VAAPI DMA-BUF, a D3D11VA
//! shared NT handle, a VideoToolbox IOSurface), which the
//! software-Vulkan CI runners do not have, and feeding them fake
//! handles would only assert that a driver rejects garbage.
//!
//! What covers the hardware paths instead:
//! - `import_dmabuf_plane` (Linux): a VAAPI machine running
//!   `quercus-render/tests/footage_import_test.rs` (DMA-BUF + DRM format
//!   modifier import); the M5 real-hardware acceptance is recorded in
//!   `docs/zh/plans/render-pipeline-threads-m5-branch-coverage.txt`.
//! - `import_d3d11_shared_texture` (Windows): the Windows job on a host
//!   with D3D11VA; a lavapipe-only runner has no shared texture to
//!   import and the decode side falls back to staging.
//! - `import_iosurface_plane` (macOS): the macOS job with VideoToolbox
//!   active.
//! - `register_imported_texture*`/`texture_view`: the alias registry
//!   and view helper are unit-tested below on a locally created array
//!   texture (no platform import); the `Plane0`/`Plane1` aspects only
//!   become valid with the multi-planar formats of a real import.

use super::*;
use crate::backend::gpu_or_skip;
use crate::error::Error;

/// The Linux plane format map accepts exactly the luma/chroma planes
/// the planar import feeds it and rejects everything else.
#[cfg(target_os = "linux")]
#[test]
fn import_vk_format_maps_luma_and_chroma_planes() {
	use ash::vk::Format;
	assert_eq!(
		import_vk_format(wgpu::TextureFormat::R8Unorm),
		Some(Format::R8_UNORM)
	);
	assert_eq!(
		import_vk_format(wgpu::TextureFormat::Rg8Unorm),
		Some(Format::R8G8_UNORM)
	);
	assert_eq!(
		import_vk_format(wgpu::TextureFormat::R16Unorm),
		Some(Format::R16_UNORM)
	);
	assert_eq!(
		import_vk_format(wgpu::TextureFormat::Rg16Unorm),
		Some(Format::R16G16_UNORM)
	);
	for rejected in [
		wgpu::TextureFormat::Rgba8Unorm,
		wgpu::TextureFormat::Rgba32Float,
		wgpu::TextureFormat::NV12,
		wgpu::TextureFormat::P010,
	] {
		assert_eq!(
			import_vk_format(rejected),
			None,
			"{rejected:?} is not a single-plane sample format"
		);
	}
}

/// The macOS plane map accepts the interleaved 8/16-bit layouts the
/// VideoToolbox import feeds and rejects everything else.
#[cfg(target_os = "macos")]
#[test]
fn iosurface_pixel_format_maps_supported_planes() {
	use objc2_metal::MTLPixelFormat;
	assert_eq!(
		iosurface_pixel_format(wgpu::TextureFormat::R8Unorm),
		Some(MTLPixelFormat::R8Unorm)
	);
	assert_eq!(
		iosurface_pixel_format(wgpu::TextureFormat::Rg8Unorm),
		Some(MTLPixelFormat::RG8Unorm)
	);
	assert_eq!(
		iosurface_pixel_format(wgpu::TextureFormat::R16Unorm),
		Some(MTLPixelFormat::R16Unorm)
	);
	assert_eq!(
		iosurface_pixel_format(wgpu::TextureFormat::Rg16Unorm),
		Some(MTLPixelFormat::RG16Unorm)
	);
	assert_eq!(
		iosurface_pixel_format(wgpu::TextureFormat::Rgba8Unorm),
		None
	);
}

/// `import_dmabuf_plane`'s argument validation must reject zero-sized
/// imports and non-plane formats *before* it touches Vulkan, so this
/// runs without any DMA-BUF: the plane fd (here invalid on purpose)
/// is never reached.
#[cfg(target_os = "linux")]
#[test]
fn dmabuf_import_rejects_invalid_arguments_without_importing() {
	let Some(ctx) = gpu_or_skip("the DMA-BUF argument validation test") else {
		return;
	};
	if ctx.kind() != crate::backend::BackendKind::Vulkan {
		eprintln!("SKIP: the DMA-BUF validation test needs a Vulkan context");
		return;
	}
	let plane = DmaBufPlane {
		fd: -1,
		offset: 0,
		pitch: 64,
		modifier: 0,
	};
	let err = ctx
		.import_dmabuf_plane(0, 64, wgpu::TextureFormat::R8Unorm, plane, None)
		.unwrap_err();
	assert!(matches!(err, Error::State), "zero width: {err:?}");
	let err = ctx
		.import_dmabuf_plane(64, 0, wgpu::TextureFormat::R8Unorm, plane, None)
		.unwrap_err();
	assert!(matches!(err, Error::State), "zero height: {err:?}");
	let err = ctx
		.import_dmabuf_plane(64, 64, wgpu::TextureFormat::Bgra8Unorm, plane, None)
		.unwrap_err();
	assert!(matches!(err, Error::Invalid), "non-plane format: {err:?}");
}

/// The registry/view helper every import shares: one array texture,
/// several aliases. A single-slice alias must produce the explicit `D2`
/// view the M5 desktop review fix requests (wgpu-core would otherwise
/// derive `D2Array` from the texture's layer count) **and bind into the
/// planar pass's `D2` bind-group layout** — the actual failure point of
/// that regression: `create_view` succeeds on a derived `D2Array`, the
/// bind group does not. Broader aliases keep the array dimension (and
/// are shown to be rejected by the `D2` layout, so the guard is
/// falsifiable), and unknown tokens are `NotFound`.
#[test]
fn imported_texture_aliases_create_valid_views_and_bind() {
	let Some(ctx) = gpu_or_skip("the imported-texture alias test") else {
		return;
	};
	// The shape a D3D11VA import registers: one 4-layer D2 texture.
	let texture = Arc::new(ctx.device.create_texture(&wgpu::TextureDescriptor {
		label: Some("quercusrender-imported-test"),
		size: wgpu::Extent3d {
			width: 8,
			height: 8,
			depth_or_array_layers: 4,
		},
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format: wgpu::TextureFormat::Rgba8Unorm,
		usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
		view_formats: &[],
	}));
	let desc = ImportedTextureDesc {
		width: 8,
		height: 8,
		array_layers: 4,
		format: wgpu::TextureFormat::Rgba8Unorm,
		aspect: wgpu::TextureAspect::All,
		layer: Some((2, 1)),
	};
	let single = ctx.register_texture_entry(texture.clone(), desc);
	let single_view = ctx.texture_view(single).expect("single-slice alias view");

	let pair = ctx.register_texture_entry(
		texture.clone(),
		ImportedTextureDesc {
			layer: Some((0, 2)),
			..desc
		},
	);
	let pair_view = ctx.texture_view(pair).expect("two-slice alias view");

	let whole = ctx.register_texture_entry(
		texture.clone(),
		ImportedTextureDesc {
			layer: None,
			..desc
		},
	);
	ctx.texture_view(whole).expect("whole-array alias view");

	assert_ne!(single, pair, "aliases get distinct tokens");
	assert_ne!(pair, whole, "aliases get distinct tokens");
	assert!(
		matches!(ctx.texture_view(u64::MAX), Err(Error::NotFound)),
		"an unregistered token must be NotFound"
	);

	// The regression guard: the planar pass creates its bind group
	// with a `D2` layout. Without `dimension: Some(D2)` the
	// single-slice alias is a derived `D2Array` and this bind group
	// fails validation, so the test must exercise bind-group creation
	// (not just `create_view`). Use the production planar layout so the
	// guard follows it if the layout changes.
	let pipeline = ctx
		.planar_yuv_pipeline()
		.expect("the planar pass pipeline builds");
	// The chroma slot must be a single-slice `D2` alias too: slice 0 of
	// the same texture serves.
	let chroma = ctx.register_texture_entry(
		texture,
		ImportedTextureDesc {
			layer: Some((0, 1)),
			..desc
		},
	);
	let chroma_view = ctx.texture_view(chroma).expect("chroma alias view");
	let uniform = ctx.device.create_buffer(&wgpu::BufferDescriptor {
		label: Some("quercusrender-imported-test-planar-params"),
		size: 64,
		usage: wgpu::BufferUsages::UNIFORM,
		mapped_at_creation: false,
	});
	let scope = ctx.device.push_error_scope(wgpu::ErrorFilter::Validation);
	let _bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
		label: Some("quercusrender-imported-test-planar-bg"),
		layout: &pipeline.layout,
		entries: &[
			wgpu::BindGroupEntry {
				binding: 0,
				resource: wgpu::BindingResource::TextureView(&single_view),
			},
			wgpu::BindGroupEntry {
				binding: 1,
				resource: wgpu::BindingResource::TextureView(&chroma_view),
			},
			wgpu::BindGroupEntry {
				binding: 2,
				resource: uniform.as_entire_binding(),
			},
		],
	});
	let error = pollster_block_on(scope.pop());
	assert!(
		error.is_none(),
		"single-slice aliases must bind into the planar pass's D2 layout \
			 (a D2Array view is rejected here): {error:?}"
	);

	// Negative control: the broader (D2Array) alias is rejected by the
	// same layout, proving mismatch in the assertion above is
	// observable through the error scope.
	let scope = ctx.device.push_error_scope(wgpu::ErrorFilter::Validation);
	let _rejected = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
		label: Some("quercusrender-imported-test-planar-bg-rejected"),
		layout: &pipeline.layout,
		entries: &[
			wgpu::BindGroupEntry {
				binding: 0,
				resource: wgpu::BindingResource::TextureView(&pair_view),
			},
			wgpu::BindGroupEntry {
				binding: 1,
				resource: wgpu::BindingResource::TextureView(&chroma_view),
			},
			wgpu::BindGroupEntry {
				binding: 2,
				resource: uniform.as_entire_binding(),
			},
		],
	});
	let error = pollster_block_on(scope.pop());
	assert!(
		error.is_some(),
		"a D2Array alias must be rejected by the D2 planar layout"
	);
}

/// Windows OpenFX zero-copy prerequisite: an `Rgba32Float` texture
/// created through the exportable path carries a live NT handle.
#[cfg(target_os = "windows")]
#[test]
fn exportable_texture_exports_opaque_win32_handle() {
	let Some(ctx) = gpu_or_skip("exportable win32 texture") else {
		return;
	};
	if !ctx.supports_gl_zerocopy() {
		eprintln!("SKIP: context is not Vulkan / Win32 external memory disabled");
		return;
	}
	let token = ctx
		.create_exportable_texture(
			64,
			64,
			wgpu::TextureFormat::Rgba32Float,
			wgpu::TextureUsages::TEXTURE_BINDING
				| wgpu::TextureUsages::RENDER_ATTACHMENT
				| wgpu::TextureUsages::COPY_SRC
				| wgpu::TextureUsages::COPY_DST,
		)
		.expect("exportable texture");
	let export = ctx.export_gl_memory(token).expect("gl memory export");
	assert_eq!(export.width(), 64);
	assert_eq!(export.height(), 64);
	assert!(export.size() >= 64 * 64 * 16);
	let handle = export.try_clone_handle().expect("duplicate opaque handle");
	assert!(handle != 0);
	ctx.destroy_texture(token);
}

/// Linux OpenFX zero-copy prerequisite: an `Rgba32Float` texture
/// created through the exportable path carries a live `OPAQUE_FD`.
/// Real GL import is covered by the OFX plugin integration test; this
/// test only proves the Vulkan export half.
#[cfg(target_os = "linux")]
#[test]
fn exportable_texture_exports_opaque_fd() {
	use std::os::fd::AsRawFd;
	let Some(ctx) = gpu_or_skip("exportable texture") else {
		return;
	};
	if !ctx.supports_gl_zerocopy() {
		eprintln!("SKIP: context is not Vulkan / zero-copy disabled");
		return;
	}
	let token = ctx
		.create_exportable_texture(
			64,
			64,
			wgpu::TextureFormat::Rgba32Float,
			wgpu::TextureUsages::TEXTURE_BINDING
				| wgpu::TextureUsages::RENDER_ATTACHMENT
				| wgpu::TextureUsages::COPY_SRC
				| wgpu::TextureUsages::COPY_DST,
		)
		.expect("exportable texture");
	let export = ctx.export_gl_memory(token).expect("gl memory export");
	assert_eq!(export.width(), 64);
	assert_eq!(export.height(), 64);
	assert!(export.size() >= 64 * 64 * 16);
	let fd = export.try_clone_fd().expect("dup opaque fd");
	assert!(fd.as_raw_fd() >= 0);
	ctx.destroy_texture(token);
}
