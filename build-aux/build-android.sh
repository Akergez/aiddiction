#!/usr/bin/env bash
# Build Aiddiction for Android and pack it into an APK.
#
#   build-aux/build-android.sh [version]
#
# AIDDICTION_PROFILE names the cargo profile to build with (`dist`).
#
# The library is cross-compiled with cargo-ndk and Gradle packs it; see
# android/README.md. The package is for arm64 phones only.
#
# Needs: ANDROID_HOME pointing at an SDK with the platform, build tools and
# NDK that android/app/build.gradle.kts names; cargo with the
# aarch64-linux-android target; cargo-ndk; a JDK, 17 or later.
#
# Signing:
#   ANDROID_KEYSTORE_B64       base64 of a PKCS#12 keystore holding the key
#   ANDROID_KEYSTORE_PASSWORD  its password
#   ANDROID_KEY_ALIAS          the key's name in it (default: aiddiction)
#
# Android installs an update only over a package signed with the same key, so
# a release has to be signed with the one key every release is signed with: a
# build that is given a version and no keystore is refused. A build without a
# version is one to try a change with, and is signed with the debug key of
# the machine it is built on.
set -Eeuo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"

released=${1:-}
version=${released:-$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)}
profile=${AIDDICTION_PROFILE:-dist}
ndk_version=$(sed -n 's/^ *ndkVersion = "\(.*\)"/\1/p' android/app/build.gradle.kts)

: "${ANDROID_HOME:?point ANDROID_HOME at the Android SDK}"
export ANDROID_NDK_HOME=${ANDROID_NDK_HOME:-$ANDROID_HOME/ndk/$ndk_version}
[[ -d $ANDROID_NDK_HOME ]] || { echo "no NDK at $ANDROID_NDK_HOME" >&2; exit 1; }

stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT

if [[ -n ${ANDROID_KEYSTORE_B64:-} ]]; then
  : "${ANDROID_KEYSTORE_PASSWORD:?ANDROID_KEYSTORE_B64 needs ANDROID_KEYSTORE_PASSWORD}"
  base64 -d <<<"$ANDROID_KEYSTORE_B64" > "$stage/release.p12"
  unset ANDROID_KEYSTORE_B64
  export ANDROID_KEYSTORE=$stage/release.p12
elif [[ -n $released ]]; then
  echo 'refusing to build a release signed with a debug key: set ANDROID_KEYSTORE_B64' >&2
  exit 1
fi

# Whatever an earlier build left: Gradle packs everything it finds there.
jni_libs=android/app/src/main/jniLibs
rm -rf -- "$jni_libs"
cargo ndk -t arm64-v8a --platform 26 -o "$jni_libs" \
  build --profile "$profile" --locked -p aiddiction --lib

(cd android && AIDDICTION_VERSION=$version ./gradlew --console=plain --quiet assembleRelease)

name=aiddiction-$version-android-aarch64
mkdir -p dist
cp -- android/app/build/outputs/apk/release/app-release.apk "dist/$name.apk"
if command -v sha256sum >/dev/null; then
  (cd dist && sha256sum "$name.apk" > "$name.apk.sha256")
else
  (cd dist && shasum -a 256 "$name.apk" > "$name.apk.sha256")
fi

echo "dist/$name.apk"
