#!/usr/bin/env bash
# Pack an already built aiddiction-server into a tarball.
#
#   cargo build --profile dist --locked -p aiddiction-server
#   build-aux/package-server.sh [version]
#
# AIDDICTION_PROFILE names another profile's build to pack (`release`).
#
# Laid out like an install prefix, as the client's tarball is. On Linux the
# installation is `bin/aiddiction-server install`, which copies the binary to
# ~/.local/bin and sets it up as a systemd user service;
# build-aux/install-server.sh downloads a release package and does exactly
# that. The server runs on Linux and macOS; it needs nothing at run time but
# git, and Node for the agents that come through npx.
set -Eeuo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"

binary=${AIDDICTION_SERVER_BINARY:-target/${AIDDICTION_PROFILE:-dist}/aiddiction-server}
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

name=aiddiction-server-$version-$os-$arch
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
prefix=$stage/$name

install -m755 -d "$prefix/bin"
install -m755 "$binary" "$prefix/bin/aiddiction-server"
install -m755 -d "$prefix/share/doc/aiddiction-server" "$prefix/share/licenses/aiddiction-server"
install -m644 data/aiddiction-server.example.toml "$prefix/share/doc/aiddiction-server/config.example.toml"
install -m644 LICENSE "$prefix/share/licenses/aiddiction-server/LICENSE"

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
