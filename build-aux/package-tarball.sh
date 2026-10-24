#!/usr/bin/env bash
# Pack an already built Aidiction into a tarball for people who do not use
# flatpak.
#
#   cargo build --profile dist --locked -p aidiction
#   build-aux/package-tarball.sh [version]
#
# AIDICTION_PROFILE names another profile's build to pack (`release`).
#
# The archive is laid out like an install prefix (bin/, share/), so unpacking
# it into ~/.local or /usr/local is the whole installation. The binary links
# against the system's fontconfig, FreeType and Wayland/X11 libraries instead
# of carrying its own: the README lists them, and a tarball that bundled them
# would be a worse flatpak.
set -Eeuo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"

app_id=app.akergez.Aidiction
binary=${AIDICTION_BINARY:-target/${AIDICTION_PROFILE:-dist}/aidiction}
arch=$(uname -m)
# The workspace version, unless the caller says otherwise.
version=${1:-$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)}

[[ -x $binary ]] || { echo "$binary is not built" >&2; exit 1; }

name=aidiction-$version-linux-$arch
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
prefix=$stage/$name

install -Dm755 "$binary" "$prefix/bin/aidiction"
install -Dm644 "data/$app_id.desktop" "$prefix/share/applications/$app_id.desktop"
install -Dm644 "data/$app_id.metainfo.xml" "$prefix/share/metainfo/$app_id.metainfo.xml"
install -Dm644 "data/icons/hicolor/scalable/apps/$app_id.svg" \
  "$prefix/share/icons/hicolor/scalable/apps/$app_id.svg"
install -Dm644 "data/icons/hicolor/symbolic/apps/$app_id-symbolic.svg" \
  "$prefix/share/icons/hicolor/symbolic/apps/$app_id-symbolic.svg"
install -Dm644 LICENSE "$prefix/share/licenses/aidiction/LICENSE"

mkdir -p dist
# Fixed owner and order, so the same build packs to the same bytes.
tar -C "$stage" --owner=0 --group=0 --numeric-owner --sort=name \
  -czf "dist/$name.tar.gz" "$name"
(cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")

echo "dist/$name.tar.gz"
