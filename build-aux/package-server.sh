#!/usr/bin/env bash
# Pack an already built aidiction-server into a tarball.
#
#   cargo build --profile dist --locked -p aidiction-server
#   build-aux/package-server.sh [version]
#
# AIDICTION_PROFILE names another profile's build to pack (`release`).
#
# Laid out like an install prefix, as the client's tarball is: unpacking it
# into ~/.local is the whole installation, and on Linux that puts the systemd
# user unit where `systemctl --user` looks (share/systemd/user). The server
# runs on Linux and macOS; it needs nothing at run time but git, and Node for
# the agents that come through npx.
set -Eeuo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"

binary=${AIDICTION_SERVER_BINARY:-target/${AIDICTION_PROFILE:-dist}/aidiction-server}
version=${1:-$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)}

[[ -x $binary ]] || { echo "$binary is not built" >&2; exit 1; }

case $(uname -s) in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  *) echo "the server is not packaged for $(uname -s)" >&2; exit 1 ;;
esac
case $(uname -m) in
  arm64) arch=aarch64 ;;
  *) arch=$(uname -m) ;;
esac

name=aidiction-server-$version-$os-$arch
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
prefix=$stage/$name

install -m755 -d "$prefix/bin"
install -m755 "$binary" "$prefix/bin/aidiction-server"
install -m755 -d "$prefix/share/doc/aidiction-server" "$prefix/share/licenses/aidiction-server"
install -m644 data/aidiction-server.example.toml "$prefix/share/doc/aidiction-server/config.example.toml"
install -m644 LICENSE "$prefix/share/licenses/aidiction-server/LICENSE"
if [[ $os == linux ]]; then
  install -m755 -d "$prefix/share/systemd/user"
  install -m644 data/aidiction-server.service "$prefix/share/systemd/user/aidiction-server.service"
fi

mkdir -p dist
if [[ $os == linux ]]; then
  # Fixed owner and order, so the same build packs to the same bytes.
  tar -C "$stage" --owner=0 --group=0 --numeric-owner --sort=name -czf "dist/$name.tar.gz" "$name"
  (cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
else
  tar -C "$stage" -czf "dist/$name.tar.gz" "$name"
  (cd dist && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")
fi

echo "dist/$name.tar.gz"
