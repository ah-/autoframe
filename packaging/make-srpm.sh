#!/bin/bash
# Builds a source RPM with the Rust dependencies vendored, so the binary RPM can be built
# offline (mock, COPR). Used by CI, COPR (.copr/Makefile) and for local builds.
#
#   packaging/make-srpm.sh [OUTDIR]          # default: dist/
#   rpmbuild --rebuild dist/autoframe-*.src.rpm
#
# Tagged commits (vX.Y.Z matching Cargo.toml) get release 1; anything else gets a snapshot
# release like 0.20261105.git1a2b3c4.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=$(realpath -m "${1:-$root/dist}")
cd "$root"

name=autoframe
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
commit=$(git rev-parse --short HEAD)
if git describe --exact-match --tags --match "v$version" >/dev/null 2>&1; then
    release=1
else
    release="0.$(git log -1 --format=%cd --date=format:%Y%m%d).git$commit"
fi

if [ ! -f third_party/libfacedetection/src/facedetectcnn.cpp ]; then
    git submodule update --init
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out" "$work/SOURCES" "$work/SPECS" "$work/SRPMS"

# Source0: all tracked files, including the libfacedetection submodule.
git ls-files -z --recurse-submodules |
    tar --null -T - --transform "s,^,$name-$version/," -czf "$work/SOURCES/$name-$version.tar.gz"

# Source1: vendored crates.
cargo vendor --locked --quiet "$work/vendor" >/dev/null
tar -C "$work" -cJf "$work/SOURCES/$name-$version-vendor.tar.xz" vendor

sed -e "s/@VERSION@/$version/g" -e "s/@RELEASE@/$release/g" -e "s/@COMMIT@/$commit/g" \
    -e "s/@DATE@/$(LC_ALL=C date -u '+%a %b %d %Y')/g" \
    packaging/$name.spec.in > "$work/SPECS/$name.spec"

rpmbuild -bs --define "_topdir $work" "$work/SPECS/$name.spec" >/dev/null
cp "$work"/SRPMS/*.src.rpm "$out/"
ls "$out"/*.src.rpm
