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
the server in use; `AIDDICTION_DEMO=2` adds a second one ("Second server",
project "Second repo") for the inbox of several servers. Never try things
against the default directories: that is the person's own server list.

A server for experiments: `AIDDICTION_LISTEN`, `AIDDICTION_DATA_DIR`,
`AIDDICTION_PROJECTS_ROOT`, `AIDDICTION_TOKEN` override its configuration, and
`--config /nonexistent` skips the file.

### Looking at the interface from a script

`AIDDICTION_SCRIPT="until:page=inbox;click:748,452;type:hello;key:enter;shot:x;quit"`
drives the window (`crates/aiddiction/src/script.rs`); `AIDDICTION_SIZE=WxH` sets
its size (below 44 rem wide is the phone layout); `AIDDICTION_INPUT=touch`
draws the window for a finger (the phone's skin: the phone scenarios say it
in their `# env:`); `swipe:x,y,dx,dy` is a
finger's swipe, `hold:x,y,dx,dy` the same finger left down and `lift` lifts
it. Scenarios with `expect:` /
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
through gpui-mobile (same rev as money-manager-ng); the rest of the Android
side — `android_main`, the Java classes, the keyboard, back, insets — is
gpui-android-glue (crate by tag, Java half from GitHub Packages, at one
version; `-PglueLocal` uses a checkout beside this one). On this aarch64 host the
NDK has no clang of its own: `build-aux/android-clang.sh` is the C compiler
and linker (host clang + the NDK's sysroot + rust-lld).

## Architecture

| Crate | Role |
|---|---|
| `aiddiction-paths` | Where both programs keep files: an XDG variable if set, else the platform's standard directory. |
| `aiddiction-api` | REST/event DTOs and the names of the proxy's ACP extensions (`_aiddiction/attach`, `_aiddiction/prompt_finished`, `_aiddiction/request_resolved`, `_meta.aiddictionSeq`) and of the adapters' `_auth/status_update`. No Tokio, no UI. `SessionSummary` carries what a list of conversations shows: busy, the agent's open questions, a preview, the last sequence number (and the card an agent was started on); `Task::session` names the thread started on a card. |
| `aiddiction-server` | The server: `config`, `store` (JSON files), `agents` (catalogue), `projects` (dirs, git clone), `worktrees` (checkouts of a project), `instances` (agent processes), `login` (an agent's terminal sign-in), `proxy` (ACP), `orchestrate` (starting an agent from MCP), `http` (routes). |
| `aiddiction-client` | REST + event socket + ACP socket on Tokio. No UI, and it must stay that way. |
| `aiddiction` | The application: a library (Android loads it) and a thin `main.rs`. |
| `aiddiction-fake-agent` | A scripted ACP agent (stdio) for tests and the demo. |
| `diffreview` | The diff review model after revdiff, with no UI: unified-diff parsing, annotations and revdiff's exact output format (and its parser), `DiffRequest` → git arguments, `git::LocalRepo`, the prompt that hands a review to an agent. The server uses it. |
| `diffreview-ui` | The review view on gpui-kit over `diffreview`: the files as gpui-kit's `Tree` (built by `diffreview::tree`), diff rows coloured by gpui-kit's tree-sitter highlighter (`highlight.rs`: both versions of the file rebuilt from the diff and parsed whole), annotations, revdiff's keys. Gets diffs through `ReviewBackend`; `LocalBackend` is git on this machine (what a standalone tool needs). Knows nothing of Aiddiction; gpui-android-glue only for the on-screen keyboard (raised on a tap, told an annotation is prose). |

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
  sequence number. `_aiddiction/attach` answers first (with the
  session's modes and options as they are now), then replays.
- The prompt is echoed into the journal as `user_message_chunk`: the client
  shows user messages only from there, never adds them itself.

`routing` decisions are pure functions of the proxy state and are unit-tested
in `proxy.rs`; keep new rules there and testable. The proxy also keeps what a
list of conversations needs (`State::follow`, `summaries`): `GET /instances`
gives every project's instances with them, and `POST
/instances/{id}/permission` answers an agent's question with no conversation
open, settling it for every client like a client's answer would. Whatever a
list shows that changes fires `InstancesChanged`.

### Orchestration from MCP

An agent already talking to the board (`/api/v1/mcp`) can start another one
on a task card: `list_agents` (with `projectId` for the model and settings
the agent last offered there), `start_agent`, `send_message`,
`list_subagents`. The capabilities in `list_agents` come from the agent's
last `initialize`, kept per agent id in `agent-profiles.json`
(`Supervisor::profile`); a changed command makes the profile stale. The
settings an agent last gave in `session/new` are read from the running
instance, not stored.

`start_agent` (`orchestrate.rs`) starts or reuses the project's instance in
the checkout asked for (`worktreeId`, the project's own when left out),
waits for its `initialize`, opens a session as the server itself(`Proxy::open_session`), applies `settings` in the order given, sends the
prompt and returns the ids — not the turn. A setting the agent refuses is
named and no prompt is sent. `requestId` makes a retry the same start.
The prompt carries the card's id and title and says to write the result on
it; the card's `session` names the thread. The thread is an ordinary
session in the common list (`SessionSummary::task_id` says which card), so
the person opens it from the inbox or from the card's "Open conversation",
answers permissions and stops it as usual; the caller only gets metadata
and reads the card for the result.

`send_message` (`Orchestrator::send`) is another turn of a conversation that
is already running: it finds the thread by `sessionId` (optionally
`projectId`/`instanceId`), refuses while the thread is busy or its agent is
not running, and sends a prompt with the same card text as `start_agent`.
No new instance or session is made, `requestId` makes a retry the same
message, and the answer carries the `usage` the context had before the new
turn. `SessionSummary::usage` (`ContextUsage`: `used`, `size`, optional
`cost`) is kept from the agent's `usage_update` (`proxy.rs`,
`State::follow`), so `list_subagents` tells an orchestrator how full each
child's context is.

A checkout the caller made itself is passed as `worktreePath` (absolute,
relative to the project, or `~/…`, read as the server's home): `Worktrees::adopt`
checks that the folder is a git
worktree of this project's repository (same `--git-common-dir`, not a
subfolder, not another repository), keeps it under an id of its own with
`managed: false` and answers with that id. The same folder is the same id on
a second call. The server never takes an adopted checkout away: removing it
forgets the id and leaves the folder and branch where they are. A worktree
is not self-contained: its `.git` file points into the project's own checkout
(`<project>/.git/worktrees/<name>`), so an agent in another container must
reach that path at the same address — mounting only the worktree folder is
not enough.

A start can carry a `goal` instead of a prompt. It goes to the adapter's
`_session/goal` extension (`Proxy::set_goal`), which Claude Code and Codex
ACP have: the agent starts pursuing it as a turn of its own, and the prompt,
when there is one, opens the objective; the card's text is appended as
usual. An adapter without the extension refuses at once and the start
fails; one that takes the goal may answer only when its turn is over, so the
answer is waited for 15 seconds and after that the goal is taken as
accepted. The turn is recorded in the journal like a prompt
(`Proxy::begin`).

The board also makes, lists and moves checkouts through their rounds:
`list_worktrees` gives ids, names, the temporary branch of the round, the
base it follows, the last result handed to the project and paths;
`create_worktree` makes a clone on a temporary branch of its own,
`aiddiction/<base>-<id>`, one per round; `get_worktree_results` takes that
branch into the project's repository (a clone fetches it, a shared worktree
already has it) and leaves the checkout without the branch, so stale
temporary branches do not pile up; `start_new_worktree_work` makes the next
round's branch from the base as the main checkout has it now. Uncommitted
work is refused by both, and a clone is not removed before its results are
taken. `remove_worktree` takes a server-made checkout away (refused while
an agent runs in it; an adopted one is only forgotten). An orchestrator
that cannot see the server's files works with ids alone. `create_worktree`
makes a **clone**: its `.git` is its own, so an agent in a container can
work and commit without reaching the project's checkout. (A shared-`.git`
worktree is what the UI's New worktree makes, for agents that run beside
the server.)

An agent started this way gets a token of its own (`Orchestrator::scope`):
it reaches `/mcp` only and only its project's board, without `list_agents`,
`list_subagents`, `start_agent` or `send_message`, and cannot delete the
card it works on.
The person's own bearer still opens everything; a separate token per
person's client is not there yet.

### Signing an agent in

The server's `initialize` declares `auth.terminal` and `elicitation.url`,
so the adapters offer every way they have. Codex signs in over ACP:
`authenticate`, and its device-code page comes as `elicitation/create`,
which the proxy sends only to the client whose `authenticate` it names
(`requestId`). Claude Code offers only `terminal` methods — a command line
for the client to run — and ACP has no request for that, so the server runs
it with pipes (`login.rs`, `/instances/{id}/login`): the chat shows its
output and passes the code back. The proxy keeps the last
`_auth/status_update` for clients that come later. In the client, an
"authentication required" (-32000) or a signed-out status brings up the
card in `ui/chat.rs` (`session::SignIn`, `crate::login`). The fake agent
needs signing in with `FAKE_AGENT_SIGN_IN=<file>`; scenarios set it with
`# env:`.

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

### Checkouts

A project has one main checkout — its own folder, whose `Worktree` id is the
project's id — and any number of git worktrees the server makes
(`worktrees.rs`, `POST /projects/{id}/worktrees`). A checkout works on one
temporary branch per round (`aiddiction/<base>-<id>`), made from `HEAD` or a
given commit, in `<projects root>/.aiddiction-worktrees/<project id>/<name>`,
outside the main checkout; its record is kept in `worktrees.json` with the
branch of the round in progress (`branch`, `None` between rounds), the base
it follows, the commit it started from and the branch last handed to the
project (`last_result`). A clone is made with `git clone --local
--no-checkout` and keeps only that branch; a shared worktree shares the
repository's objects and refs with the main checkout and has its own files
and index.

An agent instance is per `(project, worktree, agent)` (`Supervisor::start`),
and the proxy's root is the worktree, so `fs/*`, the session's `cwd` and the
agent's own command all stay in that checkout. Threads are not moved
between checkouts: a new conversation picks one (`NewSessionForm`), and
`StartInstance`/`start_agent` take a `worktreeId` (the main checkout when
left out). Removing a worktree (`DELETE /projects/{id}/worktrees/{id}`) is
refused while an instance runs there (409) or while it has uncommitted or
untracked files (409); a clone whose round was not taken is refused too
(409, `Unfinished`), since that branch lives only there. Shared worktrees
keep their branches in the repository. Deleting a project stops its agents
and takes the server-made worktrees with it.

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
- Every server in the settings is connected at once (`Shell::servers`,
  `Shell::sync_servers` after the list changes). The inbox and the new
  conversation dialog span all of them; the active one (`Shell::server`) is
  only whose projects and agents the columns list. A project's page, review
  and conversations find their server by the project (`Shell::server_of`;
  project ids are unique across servers), and opening a project makes its
  server the active one. The server's worktrees are read with its projects
  and kept fresh by `WorktreesChanged`; a project's page lists them and can
  make and remove one, and a new conversation chooses one (its main checkout
  by default).
- `server::ServerConnection` (entity) is one server's only owner of the REST
  client and the event socket. Its methods take a `done` callback; that callback runs
  *after* the entity's update (`cx.defer`), so it may update it again.
- `session::AgentSession` (entity) is one conversation: its own JSON-RPC over
  the ACP socket, reconnection with `afterSeq`, and the `thread::Thread`
  (pure, tested) the chat draws.
- The interface is written in gpui-adaptive's intents
  (`../gpui-mobile-components`, `crates/gpui-adaptive`; its `docs/` has a
  page per intent): each page is one `Screen`, lists are `Collection`s of
  `ListRow`s, forms are `Form`s, and each draws itself for a pointer or for a
  finger (`gpui_adaptive::init` in `lib.rs`: touch on Android). No page asks
  whether it is on a phone: what changes with the width or the input belongs
  in the intents, and a gap there is filled there.
- `ui/` calls no gpui-kit component. What no intent is (an agent's steps,
  plan and diff in `ui/work.rs`, the board in `ui/board.rs`) is built from
  `gpui_adaptive::prim` (`row`, `column`, `surface`, `pressable`,
  `scroll_x`, `Text`, `Markdown`, `Meter`), `cx.colors()` and `cx.scale()`.
  `crates/aiddiction/clippy.toml` disallows gpui-kit's components with the
  replacement for each; it lives in this crate because `diffreview-ui` is
  built on gpui-kit.
- `ui::shell::Shell` holds the pages in a `NavigatorState<Page>` (`Page` is a
  `Route`: a conversation's companion is the inbox, a project's the
  projects, a settings page's the settings list; with nothing open the
  inbox and the projects have a `Pick` page beside them) and the open chats;
  pages are `impl Shell` blocks in `ui/*.rs`, each returning its `Screen`.
  Three sections in `Destinations` (a rail on a desktop, tabs on a phone):
  the inbox (every conversation on every server, home), projects, settings
  (appearance, agents, servers). The navigator puts a page beside its list
  when there is room and over it when there is not; the review has no
  companion and takes the whole width. `crate::inbox` (pure, tested) groups
  the conversations by project, newest first, from the servers' summaries and
  `Settings::seen` (the last sequence number shown, per conversation);
  `ui/inbox.rs` makes each project a `Collection::section` under its name and
  each conversation a `ListRow` with its agent (and server, with several),
  which answers the agent's questions in place (`Ask` in the row's body). A new conversation
  is one form (`ui/new_session.rs`: searchable project and agent `Choice`s,
  the project's checkout, the first message, sent once the session is ready). Read the gpui-kit
  skill's Coding Guides before changing structure.
- The chat sums a run of tool calls up in one line (`thread::summarize`,
  `Thread::run_start`); its list redraws from a run's first entry. The plan
  and the agent's mode sit on the composer (gpui-adaptive's `Composer`); the bar
  has the title, Review and close.
- A board card whose `Task::session` is still running has "Open conversation"
  among its actions (`ui/project.rs`): the way into the thread an MCP
  `start_agent` made. Those threads are in the inbox like any other.
- A scroll area of one axis made with gpui's `overflow_y_scroll()` /
  `overflow_x_scroll()` also takes `.lock_scroll_axis()` (a `uniform_list`:
  `style().restrict_scroll_to_axis`); without it a finger's sideways swipe
  scrolls it along its own axis too. gpui-kit's `overflow_*_scrollbar()`
  already locks. `phone-swipe.script` checks it with `swipe:`.
- Back on a phone is the navigator's: while there is somewhere to go back
  to it is a layer of gpui-android-glue's `back`, and while predictive
  back's gesture (Android 14+) or a swipe is under way the page moves with
  the finger over the page back goes to. A finger drawn to the right goes
  back unless the page scrolls sideways (`Route::scrolls_sideways`: the
  board, the diff). Forms and confirmations open as layers of back too.
  `phone-swipe-back.script` checks it.
