# Aiddiction for Android

The application is the same Rust crate as on the desktop, built as a shared
object and loaded by a `NativeActivity`. The platform layer is
[gpui-mobile](https://github.com/longbridge/gpui-mobile); the Java classes in
`app/src/main/java/dev/gpui/mobile` and the Gradle project come from its
example (by way of money-manager-ng), under the same licence terms.

The settings — the servers and their tokens — are kept in the application's
private directory (`android_main` in `crates/aiddiction/src/lib.rs` points the
XDG variables at it). The manifest declares the network permission and
nothing else.

## What is needed

- The Android SDK with a platform (`android-36`) and build tools, and an NDK
  (r27, as `app/build.gradle.kts` names).
- `cargo install cargo-ndk`, and `rustup target add aarch64-linux-android`
  (for the toolchain `rust-toolchain.toml` names).
- A JDK, 17 or later.
- CMake: `aws-lc-sys` (rustls' crypto) is built from source.

## Building

From the repository root, the library first:

```sh
export ANDROID_HOME=$HOME/Android/Sdk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/27.0.12077973

cargo ndk -t arm64-v8a --platform 26 -o android/app/src/main/jniLibs \
    build -p aiddiction --lib --release
```

Build it `--release`: a debug library is very large, and the renderer's
debug labels crash the emulator's Vulkan driver.

and then the package:

```sh
cd android
./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

The log goes to logcat under the tag `aiddiction`.

### On an aarch64 Linux host

The NDK ships no aarch64 clang, so `cargo ndk` cannot work there; the library
is built with the host's clang through `build-aux/android-clang.sh`:

```sh
export ANDROID_NDK_HOME=$HOME/Android/ndk/android-ndk-r27c
export CC_aarch64_linux_android=$PWD/build-aux/android-clang.sh
export CXX_aarch64_linux_android=$PWD/build-aux/android-clang.sh
export AR_aarch64_linux_android=ar
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$PWD/build-aux/android-clang.sh
cargo build -p aiddiction --lib --release --target aarch64-linux-android
```

Packaging is not possible there: `aapt2` and the other build tools exist only
for x86_64. Build the package on an x86_64 machine (CI).

`build-aux/build-android.sh` does both steps with the `dist` profile and
leaves a package in `dist/`.

## Reaching a server from the phone

The server listens on `127.0.0.1` unless told otherwise. For a phone, run it
on an address the phone can reach — a VPN such as Tailscale, or the LAN —
with `listen = "0.0.0.0:7878"` in its configuration, preferably behind a TLS
proxy. Plain `http://` works (Rust's sockets do not go through Android's
network security configuration), but the token then crosses the network in
the clear.
