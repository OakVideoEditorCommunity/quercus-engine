#!/bin/sh
# Quercus CUDA -> Vulkan spike: build + run the probes and print the facts
# they depend on (project FFmpeg build gates, driver, devices).
#
# Run on the machine that owns the NVIDIA device node (the probes cannot run
# inside a container/bwrap sandbox whose /dev is a private tmpfs: there the
# CUDA/Vulkan device nodes do not exist, even when the host has them).
#
#   1. cuda_vulkan_probe   - route A (cuMemAlloc -> DMA-BUF) + route B
#                            (VMM POSIX_FD -> Vulkan OPAQUE_FD/DMA_BUF)
#                            + Vulkan Video capabilities
#   2. ffmpeg_pool_probe   - route B end-to-end with FFmpeg's CUDA
#                            AVHWFramesContext using a caller-supplied,
#                            exportable VMM pool (links project libavutil)
set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../../../.." && pwd)
TMP=${TMPDIR:-/tmp}
PROBE="$TMP/quercus_cuda_vulkan_probe"
POOLPROBE="$TMP/quercus_ffmpeg_pool_probe"

echo "== repo root: $ROOT"

echo
echo "== project FFmpeg build gates (.cache/ffmpeg-src/config.h) =="
CFG="$ROOT/.cache/ffmpeg-src/config.h"
if [ -f "$CFG" ]; then
	grep -E '^#define (CONFIG_VULKAN|CONFIG_VULKAN_1_4|CONFIG_CUDA|CONFIG_CUVID|CONFIG_NVDEC|CONFIG_NVENC|CONFIG_VAAPI|CONFIG_LIBDRM) ' "$CFG" || true
else
	echo "  (missing: run tooling/ffmpeg/build-ffmpeg.sh first)"
fi

echo
echo "== Vulkan Video symbols in the built static libs =="
if [ -f "$ROOT/.cache/ffmpeg/lib/libavcodec.a" ]; then
	printf '  h264_vulkan in libavcodec.a : %s\n' \
		"$(strings "$ROOT/.cache/ffmpeg/lib/libavcodec.a" | grep -c h264_vulkan || true)"
	printf '  hevc_vulkan in libavcodec.a : %s\n' \
		"$(strings "$ROOT/.cache/ffmpeg/lib/libavcodec.a" | grep -c hevc_vulkan || true)"
	printf '  hwcontext_vulkan in avutil : %s\n' \
		"$(strings "$ROOT/.cache/ffmpeg/lib/libavutil.a" | grep -ci hwcontext_vulkan || true)"
fi

echo
echo "== kernel / devices =="
(uname -r; ls /dev/nvidia* 2>/dev/null; ls /dev/dri 2>/dev/null) || true
(nvidia-smi --query-gpu=name,driver_version --format=csv,noheader 2>/dev/null) || \
	echo "  nvidia-smi unavailable"

echo
echo "== build cuda_vulkan_probe =="
cc -O2 -Wall -Wextra -o "$PROBE" "$HERE/cuda_vulkan_probe.c" -ldl -lvulkan
echo "  built: $PROBE"

echo
echo "== build ffmpeg_pool_probe (project libavutil) =="
if cc -O2 -Wall -Wextra -o "$POOLPROBE" "$HERE/ffmpeg_pool_probe.c" \
	-I"$ROOT/.cache/ffmpeg/include" -L"$ROOT/.cache/ffmpeg/lib" \
	-lavutil -lva-drm -lva -lvdpau -lX11 -lm -lz -ldrm -latomic -pthread -ldl -lvulkan 2>"$TMP/quercus_pool_build.err"; then
	echo "  built: $POOLPROBE"
else
	echo "  SKIP: project libavutil link failed (optional probe); see $TMP/quercus_pool_build.err"
	POOLPROBE=
fi

echo
echo "== run cuda_vulkan_probe =="
"$PROBE" || true

if [ -n "${POOLPROBE:-}" ]; then
	echo
	echo "== run ffmpeg_pool_probe =="
	"$POOLPROBE" || true
fi

# ---- NVDEC -> custom VMM pool (decisive test) -------------------------
echo
echo "== build nvdec_pool_probe (project libavformat/libavcodec) =="
NVDEC="$TMP/quercus_nvdec_pool_probe"
LIBS=$(PKG_CONFIG_PATH="$ROOT/.cache/ffmpeg/lib/pkgconfig" \
	pkg-config --static --libs libavformat libavcodec libavutil 2>/dev/null || true)
if [ -n "$LIBS" ] && cc -O2 -o "$NVDEC" "$HERE/nvdec_pool_probe.c" \
	-I"$ROOT/.cache/ffmpeg/include" -L"$ROOT/.cache/ffmpeg/lib" \
	-Wl,--start-group $LIBS -Wl,--end-group -ldl -lvulkan \
	2>"$TMP/quercus_nvdec_build.err"; then
	echo "  built: $NVDEC"
	CLIP=${QUERCUS_SPIKE_CLIP:-$TMP/quercus_routeb_clip.mp4}
	if [ ! -f "$CLIP" ] && command -v ffmpeg >/dev/null 2>&1; then
		echo "  generating test clip: $CLIP"
		ffmpeg -y -loglevel error -f lavfi -i testsrc2=size=640x360:rate=25 -t 1 \
			-pix_fmt yuv420p -c:v libx264 -profile:v high -g 10 "$CLIP" || true
	fi
	echo
	echo "== run nvdec_pool_probe ${CLIP} =="
	"$NVDEC" "$CLIP" || true
else
	echo "  SKIP: link failed (optional probe); see $TMP/quercus_nvdec_build.err"
fi
