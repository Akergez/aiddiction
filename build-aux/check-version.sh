#!/usr/bin/env bash
# A release is a tag, and the tag is the version: `v1.2.3` may only be put on
# a commit whose crates all say 1.2.3.
#
#   build-aux/check-version.sh v1.2.3
#
# Prints the bare version. CI runs it first in a tag pipeline, so a tag that
# does not match fails before anything is built or published under the wrong
# number.
set -Eeuo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"

tag=${1:?usage: ${0##*/} v<major>.<minor>.<patch>}
if [[ ! $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "'$tag' is not a release tag: expected v<major>.<minor>.<patch>, like v1.0.0" >&2
  exit 1
fi
version=${tag#v}

# --locked: a Cargo.lock that still names the previous version is the same
# mistake as a Cargo.toml that does.
crates=$(cargo metadata --no-deps --locked --format-version 1 \
  | grep -o '"name":"[^"]*","version":"[^"]*"' \
  | sed 's/"name":"\([^"]*\)","version":"\([^"]*\)"/\1 \2/')

wrong=0
while read -r name found; do
  if [[ $found != "$version" ]]; then
    echo "$name is at $found, but the tag says $version" >&2
    wrong=1
  fi
done <<<"$crates"
if (( wrong )); then
  echo "set version in [workspace.package] of Cargo.toml, run 'cargo update --workspace', commit, and tag that commit" >&2
  exit 1
fi

echo "$version"
