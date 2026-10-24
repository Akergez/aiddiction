# Aidiction

Projects and ACP agents on your own servers, from the desktop or a phone.

- **aidiction-server** keeps projects (empty folders or `git clone`s) on the
  machine it runs on and starts ACP agents in them — Claude Code and Codex out
  of the box, anything else that speaks ACP by a command line — proxying the
  conversation to clients over WebSocket.
- **aidiction** is the client: pick a server, see its projects, make one
  (empty, cloned from git, or a folder already on the server), start an
  agent in it and work with it. Light, dark, Material You colours or any
  theme from Zed's registry. Linux desktop and Android, drawn
  with [gpui-kit](https://gpui-kit.com).

The design is in `docs/PLAN.md`.

## Running a server

```sh
cargo run --release -p aidiction-server
```

On first start it prints an access token (again with
`aidiction-server token`) and keeps new projects in `~/aidiction-projects`.
Its configuration is `~/.config/aidiction-server/config.toml` and its data
`~/.local/share/aidiction-server/` (the platform's standard directories
elsewhere). It
listens on `127.0.0.1:7878`; to reach it from a phone, set `listen` in
`~/.config/aidiction-server/config.toml` to an address the phone can reach
(a VPN such as Tailscale is the easy way) and put TLS in front of it.

Claude Code and Codex run through their ACP adapters with `npx`, so Node must
be on the server's `PATH`, and each must already be signed in there
(`claude`, `codex login`). More agents go in the config file:

```toml
[[agents]]
id = "gemini"
name = "Gemini CLI"
command = "gemini"
args = ["--experimental-acp"]
```

or are added from the client ("Custom agent" on a project's page): a name, a
command line run by bash in the project's folder, and environment variables.

## Running the client

```sh
cargo run --release -p aidiction
```

Add the server with its address and token. To try everything without a
server of your own, `AIDICTION_DEMO=1` starts one with a fake agent (see
`CLAUDE.md`). Android: `android/README.md`.

## Building

A recent stable Rust (the version is pinned in `rust-toolchain.toml`) and,
on Linux, the development packages for Wayland/X11, Vulkan and fonts (the
client only; the server needs none of them):

```sh
# Debian / Ubuntu
sudo apt install pkg-config libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libxcb1-dev libvulkan-dev libfontconfig-dev libfreetype-dev

# Fedora
sudo dnf install wayland-devel libxkbcommon-devel libxkbcommon-x11-devel \
  libxcb-devel vulkan-loader-devel fontconfig-devel freetype-devel
```

```sh
cargo build --release -p aidiction -p aidiction-server   # optimised, quick to rebuild
cargo build --profile dist -p aidiction                  # what ships: thin LTO, slow
cargo test --workspace
```

## Packages

| Target | How | Result |
|--------|-----|--------|
| Server, Linux or macOS | `build-aux/package-server.sh` | `dist/aidiction-server-<v>-<os>-<arch>.tar.gz` (with a systemd user unit) |
| Client, Linux tarball | `build-aux/package-tarball.sh` | `dist/aidiction-<v>-linux-<arch>.tar.gz` |
| Client, Flatpak | `build-aux/publish-flatpak.sh` | `.flatpak-repo`; with `--site`, a signed repository and `.flatpakref` |
| Client, Windows (cross-built on Linux) | `build-aux/build-windows.sh x86_64` | `dist/aidiction-<v>-windows-x86_64.zip` |
| Client, Android | `build-aux/build-android.sh` | `dist/aidiction-<v>-android-aarch64.apk` |
| Client, macOS | `build-aux/package-macos.sh` | `dist/aidiction-<v>-macos-<arch>.dmg` |

Each script's header says what it needs; `AIDICTION_PROFILE` picks the cargo
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
keytool -genkeypair -storetype PKCS12 -keystore release.p12 -alias aidiction \
  -keyalg RSA -keysize 4096 -validity 10000
base64 -w0 release.p12   # → ANDROID_KEYSTORE_B64
```

Keep the keystore somewhere safe and out of the repository: losing it means
nobody can update an installed copy.

## License

GPL-3.0; see [LICENSE](LICENSE). The icons are [Lucide](https://lucide.dev),
ISC.
