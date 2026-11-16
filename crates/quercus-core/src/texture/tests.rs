use super::*;

#[test]
fn frame_allocate_and_linesize() {
	let mut f = Frame::new();
	let mut p = VideoParamsPod::default();
	p.width = 4;
	p.height = 3;
	f.set_video_params(p);
	assert_eq!(f.width, 4);
	assert_eq!(f.height, 3);
	assert_eq!(f.channels, 4);
	assert_eq!(f.linesize_bytes(), 4 * 4 * 4);
	assert!(f.allocate());
	assert_eq!(f.data.len(), 4 * 3 * 4 * 4);
	assert!(f.data.iter().all(|&b| b == 0));
}

#[test]
fn allocate_rejects_invalid() {
	let mut f = Frame::new();
	assert!(!f.allocate());
	f.width = 0;
	f.height = 10;
	f.channels = 4;
	f.format = PixelFormat::F32;
	assert!(!f.allocate());
}

#[test]
fn dummy_frame_semantics() {
	let d = Frame::dummy();
	assert!(d.is_dummy());
	assert_eq!(d.width, 0);
	let t = Texture::dummy();
	assert!(t.is_dummy());
	assert_eq!(t.size(), (0, 0));
	assert_eq!(t.backend(), BackendKind::Cpu);
}

#[test]
fn video_params_roundtrip_and_pointers() {
	let mut f = Frame::new();
	let mut p = VideoParamsPod::default();
	p.width = 6;
	p.height = 4;
	p.divider = 2;
	p.pixel_aspect_num = 2;
	p.pixel_aspect_den = 1;
	f.set_video_params(p);
	// Divider shrinks the buffer dims (effective resolution).
	assert_eq!(f.width, 3);
	assert_eq!(f.height, 2);
	let pod = f.video_params();
	assert_eq!(pod.width, 3);
	assert_eq!(pod.pixel_aspect_num, 2);
	assert!(f.is_float());
	assert_eq!(f.pixel_count(), 6);
	f.allocate();
	assert!(!f.data().is_null());
	assert!(!f.data_mut().is_null());
	// timestamp default null.
	assert!(f.timestamp.is_null());
}

#[test]
fn wrap_and_to_frame_roundtrip() {
	let mut f = Frame::new();
	let mut p = VideoParamsPod::default();
	p.width = 2;
	p.height = 2;
	f.set_video_params(p);
	f.allocate();
	f.data[0] = 0xAB;
	let t = Texture::wrap_frame(f.clone());
	assert_eq!(t.to_frame().unwrap(), f);
}

#[test]
fn planar_descriptor_matrix_geometry_and_formats() {
	use wgpu::TextureFormat as F;
	// 4:2:0 planar U8 (I420/YUV420P): three single-channel planes.
	let d = PlanarDescriptor::yuv_planar(Subsampling::Cs420, PlaneStorage::U8);
	assert_eq!(d.plane_count(), 3);
	assert_eq!(d.plane_size(0, 6, 4), Some((6, 4)));
	assert_eq!(d.plane_size(1, 6, 4), Some((3, 2)));
	assert_eq!(d.plane_size(2, 6, 4), Some((3, 2)));
	assert_eq!(d.plane_size(3, 6, 4), None);
	for plane in 0..3 {
		assert_eq!(d.plane_texture_format(plane), Some(F::R8Unorm));
	}
	assert_eq!(d.uv_scale(), (0.5, 0.5));
	assert_eq!(d.bit_depth(), 8);
	assert_eq!(d.plane_source_components(1), Some(1));

	// 4:2:2 planar U16 with odd dimensions: chroma half width, full
	// height, rounded up.
	let d = PlanarDescriptor::yuv_planar(Subsampling::Cs422, PlaneStorage::U16);
	assert_eq!(d.plane_size(0, 5, 3), Some((5, 3)));
	assert_eq!(d.plane_size(1, 5, 3), Some((3, 3)));
	assert_eq!(d.plane_size(2, 5, 3), Some((3, 3)));
	assert_eq!(d.plane_texture_format(0), Some(F::R16Unorm));
	assert_eq!(d.uv_scale(), (0.5, 1.0));
	assert_eq!(d.bit_depth(), 16);
	assert_eq!(d.storage.bytes_per_component(), 2);

	// 4:4:4 planar F16/F32: all three planes full size.
	let d = PlanarDescriptor::yuv_planar(Subsampling::Cs444, PlaneStorage::F16);
	assert_eq!(d.plane_size(2, 9, 7), Some((9, 7)));
	assert_eq!(d.plane_texture_format(1), Some(F::R16Float));
	assert_eq!(d.uv_scale(), (1.0, 1.0));
	let d = PlanarDescriptor::yuv_planar(Subsampling::Cs444, PlaneStorage::F32);
	assert_eq!(d.plane_texture_format(0), Some(F::R32Float));

	// 4:2:2 semi-planar U16 (NV16/P216 shape): luma + interleaved
	// two-channel chroma.
	let d = PlanarDescriptor::yuv_semi_planar(Subsampling::Cs422, PlaneStorage::U16);
	assert_eq!(d.plane_count(), 2);
	assert_eq!(d.plane_size(1, 6, 3), Some((3, 3)));
	assert_eq!(d.plane_texture_format(0), Some(F::R16Unorm));
	assert_eq!(d.plane_texture_format(1), Some(F::Rg16Unorm));
	assert_eq!(d.plane_source_components(1), Some(2));
	assert_eq!(d.plane_texture_components(1), Some(2));

	// 4:2:0 semi-planar U8/F16 (NV12 / float variants).
	let d = PlanarDescriptor::yuv_semi_planar(Subsampling::Cs420, PlaneStorage::U8);
	assert_eq!(d.plane_texture_format(1), Some(F::Rg8Unorm));
	let d = PlanarDescriptor::yuv_semi_planar(Subsampling::Cs420, PlaneStorage::F16);
	assert_eq!(d.plane_texture_format(0), Some(F::R16Float));
	assert_eq!(d.plane_texture_format(1), Some(F::Rg16Float));

	// Interleaved RGB (3 source channels expanded to a 4-channel
	// texture) and RGBA.
	let d = PlanarDescriptor::rgb(PlaneStorage::U8);
	assert_eq!(d.plane_count(), 1);
	assert_eq!(d.plane_size(0, 5, 3), Some((5, 3)));
	assert_eq!(d.plane_source_components(0), Some(3));
	assert_eq!(d.plane_texture_components(0), Some(4));
	assert_eq!(d.plane_texture_format(0), Some(F::Rgba8Unorm));
	assert_eq!(d.uv_scale(), (1.0, 1.0));
	let d = PlanarDescriptor::rgba(PlaneStorage::F32);
	assert_eq!(d.plane_source_components(0), Some(4));
	assert_eq!(d.plane_texture_format(0), Some(F::Rgba32Float));
	let d = PlanarDescriptor::rgba(PlaneStorage::U16);
	assert_eq!(d.plane_texture_format(0), Some(F::Rgba16Unorm));

	// Storage recovery round-trips every plane format.
	for storage in [
		PlaneStorage::U8,
		PlaneStorage::U16,
		PlaneStorage::F16,
		PlaneStorage::F32,
	] {
		for components in [1usize, 2, 4] {
			let format = texture_format_for(storage, components);
			assert_eq!(
				PlaneStorage::from_texture_format(format),
				Some(storage),
				"{format:?}"
			);
		}
	}
	assert_eq!(PlanarDescriptor::rgba(PlaneStorage::U8).name(), "rgba-u8");
	assert_eq!(
		PlanarDescriptor::yuv_planar(Subsampling::Cs422, PlaneStorage::U16).name(),
		"yuv-planar-422-u16"
	);
}

#[test]
fn subsampling_inference_from_dims() {
	assert_eq!(
		Subsampling::from_chroma_size(3, 2, 6, 4),
		Some(Subsampling::Cs420)
	);
	assert_eq!(
		Subsampling::from_chroma_size(3, 4, 6, 4),
		Some(Subsampling::Cs422)
	);
	assert_eq!(
		Subsampling::from_chroma_size(6, 4, 6, 4),
		Some(Subsampling::Cs444)
	);
	// Odd luma dimensions round the chroma size up.
	assert_eq!(
		Subsampling::from_chroma_size(3, 2, 5, 3),
		Some(Subsampling::Cs420)
	);
	assert_eq!(
		Subsampling::from_chroma_size(3, 3, 5, 3),
		Some(Subsampling::Cs422)
	);
	// An impossible ratio is rejected.
	assert_eq!(Subsampling::from_chroma_size(2, 2, 6, 4), None);
}

#[test]
fn planar_format_legacy_adapters() {
	use wgpu::TextureFormat as F;
	assert_eq!(
		PlanarFormat::Nv12.descriptor(),
		PlanarDescriptor::yuv_semi_planar(Subsampling::Cs420, PlaneStorage::U8)
	);
	assert_eq!(PlanarFormat::Nv12.y_format(), F::R8Unorm);
	assert_eq!(PlanarFormat::Nv12.uv_format(), F::Rg8Unorm);
	assert_eq!(PlanarFormat::Nv12.bit_depth(), 8);
	assert_eq!(PlanarFormat::Nv12.chroma_size(6, 4), (3, 2));

	assert_eq!(PlanarFormat::P010.y_format(), F::R16Unorm);
	assert_eq!(PlanarFormat::P010.uv_format(), F::Rg16Unorm);
	assert_eq!(PlanarFormat::P010.bit_depth(), 16);
	assert_eq!(PlanarFormat::P010.chroma_size(5, 3), (3, 2));

	assert_eq!(PlanarFormat::Yuv422P10.y_format(), F::R16Unorm);
	assert_eq!(PlanarFormat::Yuv422P10.uv_format(), F::Rg16Unorm);
	assert_eq!(PlanarFormat::Yuv422P10.bit_depth(), 16);
	assert_eq!(PlanarFormat::Yuv422P10.name(), "yuv422p10");
	assert_eq!(PlanarFormat::Yuv422P10.uv_scale(), (0.5, 1.0));
	assert_eq!(PlanarFormat::Yuv422P10.chroma_size(5, 3), (3, 3));
	assert!(PlanarFormat::Yuv422P10.is_422());
	assert!(!PlanarFormat::Nv12.is_422());
	assert_eq!(
		PlanarDescriptor::from(PlanarFormat::Yuv422P10),
		PlanarFormat::Yuv422P10.descriptor()
	);
	assert_eq!(
		PlanarFormat::Nv12.descriptor().legacy_format(),
		Some(PlanarFormat::Nv12)
	);
	assert_eq!(
		PlanarFormat::Yuv422P10.descriptor().legacy_format(),
		Some(PlanarFormat::Yuv422P10)
	);
	assert_eq!(
		PlanarDescriptor::yuv_planar(Subsampling::Cs444, PlaneStorage::U8).legacy_format(),
		None
	);
	assert_eq!(
		PlanarDescriptor::rgb(PlaneStorage::U8).legacy_format(),
		None
	);
}

/// A fake context that only records the tokens the [`GpuLease`]s
/// release, so the CPU-only tests can assert plane-token ownership
/// without a GPU.
#[derive(Default)]
struct CountingCtx {
	destroyed: std::sync::Mutex<Vec<u64>>,
}

impl GpuContextLike for CountingCtx {
	fn kind(&self) -> BackendKind {
		BackendKind::Cpu
	}
	fn destroy_texture(&self, token: u64) {
		self.destroyed.lock().unwrap().push(token);
	}
	fn upload(&self, _token: u64, _frame: &Frame) -> crate::error::Result<()> {
		Err(crate::error::Error::State)
	}
	fn download(&self, _token: u64) -> crate::error::Result<Frame> {
		Err(crate::error::Error::State)
	}
	fn blit(
		&self,
		_src: u64,
		_dst: u64,
		_processor: Option<&crate::color::ColorProcessor>,
	) -> crate::error::Result<()> {
		Err(crate::error::Error::State)
	}
}

#[test]
fn planar_texture_descriptor_planes_and_leases() {
	let counting = Arc::new(CountingCtx::default());
	let ctx: Arc<dyn GpuContextLike> = counting.clone();

	// Legacy two-plane constructor.
	let two = PlanarTexture::new(
		ctx.clone(),
		PlanarFormat::Nv12,
		(6, 4),
		(11, 12),
		YuvTransform::bt709_limited(),
		(1, 1),
	);
	assert_eq!(two.plane_count, 2);
	assert_eq!(two.plane_tokens(), &[11, 12]);
	assert_eq!(two.y, 11);
	assert_eq!(two.uv, 12);
	assert_eq!(two.uv_size(), (3, 2));
	assert_eq!(two.uv_scale(), (0.5, 0.5));
	assert_eq!(two.bit_depth(), 8);
	assert_eq!(two.legacy_format(), Some(PlanarFormat::Nv12));

	// Three-plane 4:4:4 descriptor.
	let three = PlanarTexture::from_descriptor(
		ctx.clone(),
		PlanarDescriptor::yuv_planar(Subsampling::Cs444, PlaneStorage::F32),
		(6, 4),
		&[21, 22, 23],
		YuvTransform::bt709_full(),
		(1, 1),
	)
	.unwrap();
	assert_eq!(three.plane_count, 3);
	assert_eq!(three.plane_tokens(), &[21, 22, 23]);
	assert_eq!(three.y, 21);
	assert_eq!(three.uv, 22);
	assert_eq!(three.uv_size(), (6, 4));
	assert_eq!(three.uv_scale(), (1.0, 1.0));
	assert_eq!(three.bit_depth(), 32);
	assert_eq!(three.legacy_format(), None);

	// One-plane RGB: the legacy aliases both point at the only plane.
	let rgb = PlanarTexture::from_descriptor(
		ctx.clone(),
		PlanarDescriptor::rgb(PlaneStorage::U8),
		(5, 3),
		&[31],
		YuvTransform::bt709_limited(),
		(1, 1),
	)
	.unwrap();
	assert_eq!(rgb.plane_count, 1);
	assert_eq!(rgb.plane_tokens(), &[31]);
	assert_eq!(rgb.y, 31);
	assert_eq!(rgb.uv, 31);
	assert_eq!(rgb.uv_scale(), (1.0, 1.0));

	// Wrong plane count / empty / invalid size are rejected.
	assert!(PlanarTexture::from_descriptor(
		ctx.clone(),
		PlanarDescriptor::rgb(PlaneStorage::U8),
		(2, 2),
		&[1, 2],
		YuvTransform::bt709_limited(),
		(1, 1),
	)
	.is_err());
	assert!(PlanarTexture::from_descriptor(
		ctx.clone(),
		PlanarDescriptor::yuv_planar(Subsampling::Cs420, PlaneStorage::U8),
		(2, 2),
		&[1, 2],
		YuvTransform::bt709_limited(),
		(1, 1),
	)
	.is_err());
	assert!(PlanarTexture::from_descriptor(
		ctx.clone(),
		PlanarDescriptor::rgb(PlaneStorage::U8),
		(0, 2),
		&[1],
		YuvTransform::bt709_limited(),
		(1, 1),
	)
	.is_err());

	// Dropping the last clone releases every plane token exactly
	// once.
	drop(two);
	drop(three);
	drop(rgb);
	let mut got = counting.destroyed.lock().unwrap().clone();
	got.sort_unstable();
	assert_eq!(got, vec![11, 12, 21, 22, 23, 31]);
}
