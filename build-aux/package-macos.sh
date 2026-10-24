#!/usr/bin/env bash
# Wrap an already built Aiddiction into "Aiddiction.app", and that into
# a disk image.
#
#   cargo build --release -p aiddiction
#   build-aux/package-macos.sh [version]
#
# AIDDICTION_PROFILE names another profile's build to pack (`dist`).
#
# The bundle is left in target/macos, to run where it was built; the disk
# image, in dist/, is what is given to somebody else. It is for the
# architecture of the machine that built it.
#
# Needs rsvg-convert (`brew install librsvg`) to draw the icon.
set -Eeuo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"

app_id=app.akergez.Aiddiction
binary=${AIDDICTION_BINARY:-target/${AIDDICTION_PROFILE:-release}/aiddiction}
# The workspace version, unless the caller says otherwise.
version=${1:-$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)}

[[ -x $binary ]] || { echo "$binary is not built" >&2; exit 1; }

app="target/macos/Aiddiction.app"
rm -rf -- "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

install -m755 "$binary" "$app/Contents/MacOS/aiddiction"
sed "s/@VERSION@/$version/g" data/macos/Info.plist > "$app/Contents/Info.plist"

# The icon, at every size the Dock and the Finder ask for.
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
iconset=$stage/Aiddiction.iconset
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  rsvg-convert -w "$size" -h "$size" \
    "data/icons/hicolor/scalable/apps/$app_id.svg" -o "$iconset/icon_${size}x${size}.png"
  rsvg-convert -w $((size * 2)) -h $((size * 2)) \
    "data/icons/hicolor/scalable/apps/$app_id.svg" -o "$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Aiddiction.icns"

# Signed with no identity: what an Apple silicon Mac insists on before it runs
# anything at all. It is not a developer's signature, so a copy that came
# through a browser is still held back until its quarantine mark is taken off
# (`xattr -dr com.apple.quarantine`).
codesign --force --sign - "$app"

# The disk image: the application beside a link to where it is dragged.
case $(uname -m) in
  arm64) arch=aarch64 ;;
  *) arch=$(uname -m) ;;
esac
name=aiddiction-$version-macos-$arch
mkdir -p "$stage/image" dist
cp -R -- "$app" "$stage/image/"
ln -s /Applications "$stage/image/Applications"
hdiutil create -quiet -ov -volname "Aiddiction" -srcfolder "$stage/image" \
  -format UDZO "dist/$name.dmg"
(cd dist && shasum -a 256 "$name.dmg" > "$name.dmg.sha256")

echo "$app"
echo "dist/$name.dmg"
