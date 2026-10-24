#!/bin/sh
# A C compiler and linker for aarch64 Android on a host the NDK has no
# prebuilt clang for (an aarch64 Linux machine): the host's own clang,
# pointed at the NDK's sysroot, linking with the toolchain's rust-lld.
# What matterfast's `android-clang` does, with the paths taken from the
# environment:
#
#   ANDROID_NDK_HOME   the NDK (its sysroot and libunwind are all that is used)
#
#   export CC_aarch64_linux_android=$PWD/build-aux/android-clang.sh
#   export AR_aarch64_linux_android=ar
#   export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$PWD/build-aux/android-clang.sh
prebuilt=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64
sysroot=$prebuilt/sysroot
# libunwind, which Rust's std links against, lives with the NDK's clang.
runtime=$(echo "$prebuilt"/lib/clang/*/lib/linux/aarch64)
gcc_ld=$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/gcc-ld
exec clang --target=aarch64-linux-android26 --sysroot="$sysroot" -Wno-unused-command-line-argument -fuse-ld=lld -B"$gcc_ld" -L"$runtime" "$@"
