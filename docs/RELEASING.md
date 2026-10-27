# Packages and releases

| Target | How | Result |
|--------|-----|--------|
| Server, Linux or macOS | `build-aux/package-server.sh` | `dist/aiddiction-server-<v>-<os>-<arch>.tar.gz`; `build-aux/install-server.sh` installs one as a service |
| Client, Linux tarball | `build-aux/package-tarball.sh` | `dist/aiddiction-<v>-linux-<arch>.tar.gz` |
| Client, Flatpak | `build-aux/publish-flatpak.sh` | `.flatpak-repo`; with `--site`, a signed repository and `.flatpakref` |
| Client, Windows (cross-built on Linux) | `build-aux/build-windows.sh x86_64` | `dist/aiddiction-<v>-windows-x86_64.zip` |
| Client, Android | `build-aux/build-android.sh` | `dist/aiddiction-<v>-android-aarch64.apk` |
| Client, macOS | `build-aux/package-macos.sh` | `dist/aiddiction-<v>-macos-<arch>.dmg` |

Each script's header says what it needs; `AIDDICTION_PROFILE` picks the cargo
profile (`dist` by default). `android/README.md` covers the Android build.

## Continuous integration

`.github/workflows/ci.yml` (the same pipeline as money-manager-ng and
matterfast) tests every push — unit tests, the server's end-to-end test and
the UI scenarios under a headless sway — builds every package as a workflow
artifact, and publishes only from a version tag (`v1.2.3`) whose number
every crate carries; `build-aux/check-version.sh` enforces it. The version
lives once, in `[workspace.package]` of `Cargo.toml`. A tag publishes the
flatpak repository to GitHub Pages and a release with the client's and the
server's packages.

A release needs these repository secrets:

| Secret | For |
|--------|-----|
| `ANDROID_KEYSTORE_B64`, `ANDROID_KEYSTORE_PASSWORD` | Signing the Android package |
| `FLATPAK_GPG_ID`, `FLATPAK_GPG_KEY_B64`, `FLATPAK_GPG_PASSPHRASE` | Signing the flatpak repository |

### Signing the Android package

Android installs an update only over a package signed with the same key, so
every release is signed with one key, made once:

```sh
keytool -genkeypair -storetype PKCS12 -keystore release.p12 -alias aiddiction \
  -keyalg RSA -keysize 4096 -validity 10000
base64 -w0 release.p12   # → ANDROID_KEYSTORE_B64
```

Keep the keystore somewhere safe and out of the repository: losing it means
nobody can update an installed copy.

