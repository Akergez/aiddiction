#!/usr/bin/env bash
# Cross-compile Aidiction for Windows on Linux and pack it into a zip.
#
#   build-aux/build-windows.sh x86_64 [version]
#
# No Windows machine is involved. What stands in for one:
#
# * cargo-xwin, which downloads the MSVC runtime and the Windows SDK's
#   headers and libraries and points clang-cl and lld-link at them;
# * `fxc.exe`, Microsoft's shader compiler, run through wine. GPUI ships its
#   Direct3D shaders precompiled, and nothing else produces the same bytecode.
#   It is taken from Microsoft's own NuGet package at build time and is not
#   redistributed. GPUI's build script only does this on a Windows host, so
#   build-aux/patches/ holds a patch that is applied to cargo's unpacked copy
#   of the crate before building — in place, in $CARGO_HOME, which keeps
#   Cargo.toml and Cargo.lock exactly as the Linux build has them. The patch
#   names GPUI's version: update it when `gpui-kit` is updated.
#
# Needs on PATH: cargo with the target installed, cargo-xwin, clang, lld,
# llvm, nasm, wine, git, curl, unzip, zip. CI uses the `messense/cargo-xwin`
# image and installs the rest.
set -Eeuo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"

arch=${1:?usage: ${0##*/} x86_64 [version]}
case $arch in
  x86_64) target=$arch-pc-windows-msvc ;;
  *) echo "unknown architecture: $arch" >&2; exit 2 ;;
esac
version=${2:-$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)}
profile=${AIDICTION_PROFILE:-dist}
tools=${AIDICTION_WINDOWS_TOOLS:-$root/target/windows-tools}

# The SDK package fxc.exe comes in. Pinned: the shaders are part of what
# ships, and their compiler should not change underneath a release.
sdk_version=10.0.26100.1742
sdk_bin=c/bin/10.0.26100.0

if [[ -z ${GPUI_FXC_PATH:-} ]]; then
  # wine runs the fxc.exe built for the machine it is on.
  case $(uname -m) in
    x86_64) fxc_arch=x64 ;;
    aarch64|arm64) fxc_arch=arm64 ;;
    *) echo "no fxc.exe for a $(uname -m) host" >&2; exit 1 ;;
  esac
  fxc_dir=$tools/fxc-$sdk_version-$fxc_arch
  if [[ ! -f $fxc_dir/fxc.exe ]]; then
    mkdir -p -- "$fxc_dir"
    curl --fail --silent --show-error --location --output "$tools/sdk.nupkg" \
      "https://www.nuget.org/api/v2/package/Microsoft.Windows.SDK.CPP/$sdk_version"
    # fxc.exe loads the compiler itself from the DLL beside it.
    unzip -q -j -o "$tools/sdk.nupkg" \
      "$sdk_bin/$fxc_arch/fxc.exe" "$sdk_bin/$fxc_arch/d3dcompiler_47.dll" -d "$fxc_dir"
    rm -f -- "$tools/sdk.nupkg"
  fi
  export GPUI_FXC_PATH=$fxc_dir/fxc.exe
fi
export WINEDEBUG=${WINEDEBUG:--all}
export WINEPREFIX=${WINEPREFIX:-$tools/wine}

# cargo-xwin's environment, taken as variables so it can be added to: the
# wrapper itself replaces CFLAGS and RUSTFLAGS instead of extending them.
eval "$(cargo xwin env --target "$target")"
# It also exports an empty RUSTFLAGS, and to cargo a RUSTFLAGS that is set at
# all, even to nothing, replaces the per-target flags below.
[[ -n ${RUSTFLAGS:-} ]] || unset RUSTFLAGS

# The C runtime linked in, so the .exe runs on a machine that has never seen
# the Visual C++ redistributable.
rustflags=CARGO_TARGET_$(tr 'a-z-' 'A-Z_' <<<"$target")_RUSTFLAGS
export "$rustflags=${!rustflags:-} -C target-feature=+crt-static"

# GPUI's Windows backend, as cargo unpacked it, with the shader patch on top.
cargo fetch --locked --target "$target"
gpui_patch=$root/build-aux/patches/gpui-pre-windows-0.3.7-cross-shaders.patch
gpui_src=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -maxdepth 2 -type d \
  -name gpui-pre-windows-0.3.7 -print -quit)
[[ -n $gpui_src ]] || { echo 'gpui-pre-windows 0.3.7 is not in the cargo registry: was GPUI updated?' >&2; exit 1; }
if ! grep -q GPUI_FXC_RUNNER "$gpui_src/build.rs"; then
  (cd -- "$gpui_src" && git apply "$gpui_patch")
fi

cargo build --profile "$profile" --locked -p aidiction --target "$target"

name=aidiction-$version-windows-$arch
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
mkdir -p -- "$stage/$name"
cp -- "${CARGO_TARGET_DIR:-target}/$target/$profile/aidiction.exe" "$stage/$name/"
cp -- LICENSE "$stage/$name/LICENSE.txt"

mkdir -p dist
rm -f -- "dist/$name.zip"
(cd -- "$stage" && zip -q -r -X "$root/dist/$name.zip" "$name")
(cd dist && sha256sum "$name.zip" > "$name.zip.sha256")

echo "dist/$name.zip"
