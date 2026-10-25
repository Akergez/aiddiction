#!/bin/sh
# Install aiddiction-server as a systemd user service, in one command:
#
#   curl -fsSL https://github.com/Akergez/aiddiction/releases/latest/download/install-server.sh | sh
#   curl -fsSL …/install-server.sh | sh -s -- --linger    # keep running after logout
#   curl -fsSL …/install-server.sh | sh -s -- --listen 0.0.0.0:7878   # reachable from a phone
#
# Downloads the release's server package for this machine, checks its
# checksum, and hands over to `aiddiction-server install`, which puts the
# binary in ~/.local/bin, writes ~/.config/systemd/user/aiddiction-server.service
# with this shell's PATH (so the agents find Node and git), starts it, and
# prints the address and the token. Run it again to update.
#
# AIDDICTION_VERSION=1.2.3 installs that release instead of the latest;
# AIDDICTION_REPO=owner/name downloads from a fork; AIDDICTION_DOWNLOAD_BASE
# names another place the packages are (a test's local HTTP server).
set -eu

repo=${AIDDICTION_REPO:-Akergez/aiddiction}

[ "$(uname -s)" = Linux ] || { echo "the service needs systemd: Linux only" >&2; exit 1; }
case $(uname -m) in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) echo "no server package for $(uname -m)" >&2; exit 1 ;;
esac
command -v curl >/dev/null || { echo "curl is needed" >&2; exit 1; }
command -v sha256sum >/dev/null || { echo "sha256sum is needed" >&2; exit 1; }

version=${AIDDICTION_VERSION:-}
if [ -z "$version" ]; then
  # The tag of the latest release, without its `v`.
  version=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" \
    | sed -n 's/^ *"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)
  [ -n "$version" ] || { echo "could not find the latest release of $repo" >&2; exit 1; }
fi

name=aiddiction-server-$version-linux-$arch
base=${AIDDICTION_DOWNLOAD_BASE:-https://github.com/$repo/releases/download/v$version}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

echo "downloading $name"
curl -fsSL -o "$work/$name.tar.gz" "$base/$name.tar.gz"
curl -fsSL -o "$work/$name.tar.gz.sha256" "$base/$name.tar.gz.sha256"
(cd "$work" && sha256sum -c --quiet "$name.tar.gz.sha256")
tar -xzf "$work/$name.tar.gz" -C "$work"

"$work/$name/bin/aiddiction-server" install "$@"
