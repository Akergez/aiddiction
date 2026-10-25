# Aiddiction

Projects and ACP agents on your own servers, from the desktop or a phone.

- **aiddiction-server** keeps projects (empty folders or `git clone`s) on the
  machine it runs on and starts ACP agents in them — Claude Code and Codex out
  of the box, anything else that speaks ACP by a command line — proxying the
  conversation to clients over WebSocket.
- **aiddiction** is the client: pick a server, see its projects, make one
  (empty, cloned from git, or a folder already on the server), start an
  agent in it and work with it, and review what it changed: annotate the
  diff line by line, as in [revdiff](https://github.com/umputun/revdiff),
  and send the annotations back to the agent. Light, dark, Material You colours or any
  theme from Zed's registry. Linux desktop and Android, drawn
  with [gpui-kit](https://gpui-kit.com).

The design is in `docs/PLAN.md`.

## Running a server

On Linux, as a systemd user service, in one command (run it again to
update):

```sh
curl -fsSL https://github.com/Akergez/aiddiction/releases/latest/download/install-server.sh | sh
# reachable from a phone, and running after you log out:
curl -fsSL https://github.com/Akergez/aiddiction/releases/latest/download/install-server.sh | sh -s -- --listen 0.0.0.0:7878 --linger
```

`--listen` writes the address into `~/.config/aiddiction-server/config.toml`
(made from the commented example on the first install); the same flags work
with `aiddiction-server install`.

From a checkout, the same with the server built here:

```sh
cargo run --release -p aiddiction-server -- install [--linger]
```

Either way it ends up in `~/.local/bin` and
`~/.config/systemd/user/aiddiction-server.service`, with the PATH of the shell
it was installed from (the agents need Node and git from it), and prints the
address and the token. `aiddiction-server uninstall` removes the service and
keeps the data. To just run it in a terminal:

```sh
cargo run --release -p aiddiction-server
```

On first start it prints an access token (again with
`aiddiction-server token`) and keeps new projects in `~/aiddiction-projects`.
Its configuration is `~/.config/aiddiction-server/config.toml` and its data
`~/.local/share/aiddiction-server/` (the platform's standard directories
elsewhere). It
listens on `127.0.0.1:7878`; to reach it from a phone, set `listen` in
`~/.config/aiddiction-server/config.toml` to an address the phone can reach
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
cargo run --release -p aiddiction
```

Add the server with its address and token. To try everything without a
server of your own, `AIDDICTION_DEMO=1` starts one with a fake agent (see
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
cargo build --release -p aiddiction -p aiddiction-server   # optimised, quick to rebuild
cargo build --profile dist -p aiddiction                  # what ships: thin LTO, slow
cargo test --workspace
```

## Packages

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

## License

GPL-3.0; see [LICENSE](LICENSE). The icons are [Lucide](https://lucide.dev),
ISC.
