#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/dist"
STAGE="$OUT/stage"
ARCH="$(dpkg --print-architecture)"
mkdir -p "$STAGE/usr/lib/quercus" "$STAGE/usr/include" "$OUT"

cp "$ROOT/target/release/libquercus_engine.so" "$STAGE/usr/lib/quercus/"
cp "$ROOT/target/release/libquercus_engine.a" "$STAGE/usr/lib/quercus/"
cp "$ROOT/crates/quercus-engine/include/quercus_engine.h" "$STAGE/usr/include/"

GLIBC_VERSION="$(ldd --version | sed -n '1s/.* //p')"
printf 'glibc %s\n' "$GLIBC_VERSION" > "$STAGE/GLIBC_VERSION"
tar -czf "$OUT/quercus-engine-native-linux-${ARCH}-glibc${GLIBC_VERSION}.tar.gz" -C "$STAGE" .
(
	cd "$STAGE"
	zip -qr "$OUT/quercus-engine-native-linux-${ARCH}-glibc${GLIBC_VERSION}.zip" .
)
