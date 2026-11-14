#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
OUT="$ROOT/dist"
STAGE="$OUT/stage"
ARCH="$(dpkg --print-architecture)"
mkdir -p "$STAGE/usr/lib/quercus" "$STAGE/usr/include" "$OUT"

cp "$ROOT/target/release/libquercus_engine.so" "$STAGE/usr/lib/quercus/"
cp "$ROOT/target/release/libquercus_engine.a" "$STAGE/usr/lib/quercus/"
cp "$ROOT/crates/quercus-engine/include/quercus_engine.h" "$STAGE/usr/include/"

cat > "$STAGE/DEBIAN.control" <<EOF
Package: quercus-engine
Version: $VERSION
Section: libs
Priority: optional
Architecture: $ARCH
Maintainer: Oak Team
Description: Quercus headless nonlinear video editing engine
EOF

mkdir -p "$STAGE/DEBIAN"
mv "$STAGE/DEBIAN.control" "$STAGE/DEBIAN/control"
SHLIBDEPS_ROOT="$OUT/shlibdeps"
mkdir -p "$SHLIBDEPS_ROOT/debian"
sed '1iSource: quercus-engine' "$STAGE/DEBIAN/control" > "$SHLIBDEPS_ROOT/debian/control"
DEPENDS="$(cd "$SHLIBDEPS_ROOT" && dpkg-shlibdeps --ignore-missing-info -O "$STAGE/usr/lib/quercus/libquercus_engine.so" | sed 's/^shlibs:Depends=//')"
if [ -n "$DEPENDS" ]; then
	printf 'Depends: %s\n' "$DEPENDS" >> "$STAGE/DEBIAN/control"
fi
dpkg-deb --build "$STAGE" "$OUT/quercus-engine_${VERSION}_${ARCH}.deb"

RPMROOT="$OUT/rpm"
mkdir -p "$RPMROOT"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
cat > "$RPMROOT/SPECS/quercus-engine.spec" <<EOF
Name: quercus-engine
Version: $VERSION
Release: 1%{?dist}
Summary: Quercus headless nonlinear video editing engine
License: GPL-3.0-or-later

%description
Quercus headless nonlinear video editing engine and its C ABI and static library.

%files
/usr/lib/quercus/libquercus_engine.so
/usr/lib/quercus/libquercus_engine.a
/usr/include/quercus_engine.h
EOF
RPM_STAGE="$OUT/rpm-stage"
mkdir -p "$RPM_STAGE"
cp -a "$STAGE/usr" "$RPM_STAGE/"
rpmbuild --define "_topdir $RPMROOT" --buildroot "$RPM_STAGE" -bb "$RPMROOT/SPECS/quercus-engine.spec"
cp "$RPMROOT"/RPMS/*/*.rpm "$OUT/" 2>/dev/null || cp "$RPMROOT"/RPMS/*.rpm "$OUT/"
GLIBC_VERSION="$(ldd --version | sed -n '1s/.* //p')"
printf 'glibc %s\n' "$GLIBC_VERSION" > "$STAGE/GLIBC_VERSION"
tar -czf "$OUT/quercus-engine-native-linux-${ARCH}-glibc${GLIBC_VERSION}.tar.gz" -C "$STAGE" .
(
	cd "$STAGE"
	zip -qr "$OUT/quercus-engine-native-linux-${ARCH}-glibc${GLIBC_VERSION}.zip" .
)
