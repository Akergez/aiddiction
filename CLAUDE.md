# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Aiddiction is a client-server orchestrator for ACP agents. The server (Rust,
axum) keeps projects — directories under its projects root, empty or cloned
from git — and runs ACP agents in them as child processes, proxying ACP to
clients over WebSocket. The client (Rust, GPUI through `gpui-kit`) runs on
the Linux desktop and on Android. `docs/PLAN.md` (in Russian) is the design
and its section 11 the state of the prototype.

## Commands

```sh
cargo build --workspace                       # everything; the demo needs all three binaries
cargo test --workspace                        # unit tests + the server/fake-agent/client end-to-end test
cargo test -p aiddiction-server end_to_end     # just the end-to-end one
cargo run -p aiddiction-server                 # a real server: ~/aiddiction-projects, token printed on first run
cargo run -p aiddiction-server -- token        # print the token
cargo run -p aiddiction-server -- install      # as a systemd user service (real: it starts it)
cargo clippy --workspace --all-targets
```

### Running without touching real settings

```sh
XDG_DATA_HOME=/tmp/aid/data XDG_CONFIG_HOME=/tmp/aid/config \
AIDDICTION_DEMO=1 cargo run -p aiddiction
```

`AIDDICTION_DEMO` starts `aiddiction-server` from beside the executable on
127.0.0.1:7979 with token `demo` and the fake agent configured, and makes it
the server in use. Never try things against the default directories: that is
the person's own server list.

A server for experiments: `AIDDICTION_LISTEN`, `AIDDICTION_DATA_DIR`,
`AIDDICTION_PROJECTS_ROOT`, `AIDDICTION_TOKEN` override its configuration, and
`--config /nonexistent` skips the file.

### Looking at the interface from a script

`AIDDICTION_SCRIPT="until:page=home;click:261,121;type:Demo;key:enter;shot:x;quit"`
drives the window (`crates/aiddiction/src/script.rs`); `AIDDICTION_SIZE=WxH` sets
its size (below 44 rem wide is the phone layout). Scenarios with `expect:` /
`until:` checks live in `crates/aiddiction/tests/ui/*.script`; run them
headless in the sway container, screenshots go to `target/ui-shots/`:

```sh
cargo build --workspace
podman run --rm --userns=keep-id --security-opt label=disable \
  -v "$PWD":/src -v "$HOME/.cargo/registry":"$HOME/.cargo/registry":ro -w /src \
  localhost/aiddiction-ui build-aux/ui-tests.sh --headless [scenario …]
```

Click coordinates are for the scenario's `# size:`, read off a headless
screenshot. When a scripted run checks a change, keep it as a scenario.
`localhost/aiddiction-ui` is matterfast's image with git added
(`build-aux/ui-tests.Containerfile`; build it with the command in its header).

### Packaging and CI

`.github/workflows/ci.yml` is money-manager-ng's pipeline with the names
changed, plus `server-linux` / `server-macos` jobs: tests and UI scenarios on
every push, every package as an artifact, and a release (flatpak to Pages,
packages attached) only from a `vX.Y.Z` tag that `build-aux/check-version.sh`
agrees with. Packaging scripts are in `build-aux/` (`package-server.sh` and
`install-server.sh` are ours; the rest follow money-manager-ng's). The
service itself is written by `aiddiction-server install`
(`crates/aiddiction-server/src/install.rs`), not shipped as a file: the unit
carries the installing shell's PATH so the agents find Node. To try it
without touching the real session, point `XDG_CONFIG_HOME`, `XDG_DATA_HOME`
and `XDG_BIN_HOME` at a temporary directory: everything is written there and
`systemctl --user enable` then fails, as it should. The Windows build cross-compiles
with cargo-xwin and patches GPUI's build script
(`build-aux/patches/gpui-pre-windows-0.3.7-cross-shaders.patch`): update the
patch when `gpui-kit` is updated. The client's desktop file, metainfo, icons
and macOS plist are in `data/`; the Windows icon is
`crates/aiddiction/windows/aiddiction.ico` (regenerate it with the command in
`crates/aiddiction/build.rs`). The server is Unix-only, and so is the client's
demo mode.

### Android

`android/README.md`. The library is a `cdylib` loaded by a `NativeActivity`
through gpui-mobile (same rev as money-manager-ng). On this aarch64 host the
NDK has no clang of its own: `build-aux/android-clang.sh` is the C compiler
and linker (host clang + the NDK's sysroot + rust-lld).

## Architecture

| Crate | Role |
|---|---|
| `aiddiction-paths` | Where both programs keep files: an XDG variable if set, else the platform's standard directory. |
| `aiddiction-api` | REST/event DTOs and the names of the proxy's ACP extensions (`_aiddiction/attach`, `_aiddiction/prompt_finished`, `_aiddiction/request_resolved`, `_meta.aiddictionSeq`). No Tokio, no UI. |
| `aiddiction-server` | The server: `config`, `store` (JSON files), `agents` (catalogue), `projects` (dirs, git clone), `instances` (agent processes), `proxy` (ACP), `http` (routes). |
| `aiddiction-client` | REST + event socket + ACP socket on Tokio. No UI, and it must stay that way. |
| `aiddiction` | The application: a library (Android loads it) and a thin `main.rs`. |
| `aiddiction-fake-agent` | A scripted ACP agent (stdio) for tests and the demo. |
| `diffreview` | The diff review model after revdiff, with no UI: unified-diff parsing, annotations and revdiff's exact output format (and its parser), `DiffRequest` → git arguments, `git::LocalRepo`, the prompt that hands a review to an agent. The server uses it. |
| `diffreview-ui` | The review view on gpui-kit over `diffreview`: the files as gpui-kit's `Tree` (built by `diffreview::tree`), diff rows coloured by gpui-kit's tree-sitter highlighter (`highlight.rs`: both versions of the file rebuilt from the diff and parsed whole), annotations, revdiff's keys. Gets diffs through `ReviewBackend`; `LocalBackend` is git on this machine (what a standalone tool needs). Knows nothing of Aiddiction. |

### The proxy is the agent's ACP client

`crates/aiddiction-server/src/proxy.rs` is the heart of it, and its rules are
deliberate (most come from multiagent-ui's `acp-ws-bridge.mjs`):

- The server sends `initialize` once per agent process; clients' `initialize`
  is answered from that, so a client can reconnect without restarting the
  agent. `fs/*` requests are served on the server, inside the project.
- `cwd` in `session/new|load|resume` is always rewritten to the project.
- Client request ids are rewritten to proxy-unique ones and restored on the
  way back; a response goes only to the socket that asked.
- A session belongs to the socket that last drove it; its notifications go
  there (everyone, when the owner is gone) and into a bounded journal with a
  sequence number. `_aiddiction/attach` answers first, then replays.
- The prompt is echoed into the journal as `user_message_chunk`: the client
  shows user messages only from there, never adds them itself.

`routing` decisions are pure functions of the proxy state and are unit-tested
in `proxy.rs`; keep new rules there and testable.

### Review

`diffreview` + `diffreview-ui` are meant to be reused as they are by a
standalone local review tool, so they never depend on Aiddiction's crates.
In Aiddiction the server runs `diffreview::git::LocalRepo` in the project
(`POST /projects/{id}/review/diff`, `GET /projects/{id}/review/refs`), the
client's `review::ServerBackend` implements `ReviewBackend` over REST, and
"Send to agent" (`ui/review.rs`) prompts the project's last ready
conversation with `diffreview::prompt::for_agent(output)` and clears the
review: revdiff's annotate → fix → review loop. The output format must stay
byte-for-byte revdiff's (`annotation.rs` tests pin it).

The demo makes a sample git repository with uncommitted changes as its first
project (`demo.rs`), so scenarios count it among the projects.

### Projects

A project is a folder: made under the projects root (empty or `git clone`),
or an existing folder adopted as it is (`Project::path` set). Deleting a
project removes a folder only if the server made it.

### Appearance

The theme engine is the libraries money-manager-ng and matterfast use —
`gpui-adaptive-colors` (the Material You scheme) and `gpui-zed-themes`
(Zed themes, its `Browser`, `Store`, `Registry`). Do not copy their code into
the app: `appearance.rs` only stores choices in `Settings` and calls them,
and `ui/settings_page.rs` lays out the page. `AIDDICTION_COLOR=#rrggbb` pins
the colour (the UI tests set it); `AIDDICTION_THEMES_API` replaces Zed's
registry.

### Client state

- `runtime.rs` is the only bridge to Tokio (after matterfast): `spawn`,
  `receive`, `after`. Never touch an entity from a Tokio task.
- `settings::Settings` (GPUI global over `settings.json`, mode 0600): servers
  and the active one.
- `server::ServerConnection` (entity) is the one owner of the REST client and
  the event socket. Its methods take a `done` callback; that callback runs
  *after* the entity's update (`cx.defer`), so it may update it again.
- `session::AgentSession` (entity) is one conversation: its own JSON-RPC over
  the ACP socket, reconnection with `afterSeq`, and the `thread::Thread`
  (pure, tested) the chat draws.
- `ui::shell::Shell` holds the page and the open chats; pages are `impl Shell`
  blocks in `ui/*.rs`. Read the gpui-kit skill's Coding Guides before changing
  structure.
