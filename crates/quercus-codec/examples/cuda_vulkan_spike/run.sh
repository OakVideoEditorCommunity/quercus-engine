#!/bin/sh
# Quercus CUDA -> Vulkan spike: build + run the capability probe and print
# the facts it depends on (project FFmpeg build gates, driver, devices).
#
# Run on the machine that owns the NVIDIA device node (a normal desktop
# session, not a container without /dev/nvidia*). Read-only with respect to
# the engine: it compiles one standalone C file outside cargo.
set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/../../../.." && pwd)
OUT=${TMPDIR:-/tmp}/quercus_cuda_vulkan_probe

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
(uname -r && ls /dev/nvidia* 2>/dev/null && ls /dev/dri 2>/dev/null) || true
(nvidia-smi --query-gpu=name,driver_version --format=csv,noheader 2>/dev/null) || \
	echo "  nvidia-smi unavailable"

echo
echo "== build probe =="
cc -O2 -o "$OUT" "$HERE/cuda_vulkan_probe.c" -ldl -lvulkan
echo "  built: $OUT"

echo
echo "== run probe =="
"$OUT"
