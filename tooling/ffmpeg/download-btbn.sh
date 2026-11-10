#!/usr/bin/env bash
set -euo pipefail

target="${1:-linux64}"
prefix="${2:-$PWD/.cache/ffmpeg}"
asset="ffmpeg-n8.1-latest-${target}-gpl-shared-8.1.tar.xz"
base="https://github.com/BtbN/FFmpeg-Builds/releases/download/latest"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl -fL --retry 3 --output "$work/checksums.sha256" "$base/checksums.sha256"
expected="$(awk -v asset="$asset" '$2 == asset { print $1; exit }' "$work/checksums.sha256")"
test -n "$expected"
curl -fL --retry 3 --output "$work/$asset" "$base/$asset"
actual="$(sha256sum "$work/$asset" | awk '{ print $1 }')"
test "$actual" = "$expected"

rm -rf "$prefix" "$prefix-extract"
mkdir -p "$prefix-extract"
tar -xJf "$work/$asset" -C "$prefix-extract"
inner="$(find "$prefix-extract" -mindepth 1 -maxdepth 1 -type d | head -1)"
mv "$inner" "$prefix"
