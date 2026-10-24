#!/usr/bin/env bash
# Play every scenario in crates/aiddiction/tests/ui/ into the real application
# (in demo mode: its own server with the fake agent) and fail if one does not
# come out the way it says it should. After matterfast's harness.
#
#   build-aux/ui-tests.sh                 on the display you are sitting at
#   build-aux/ui-tests.sh --headless      under a private headless sway
#   build-aux/ui-tests.sh [--headless] first-chat   only these
#
# A scenario is an AIDDICTION_SCRIPT (crates/aiddiction/src/script.rs), one step
# a line, with `# size: 1100x760` in its header. Screenshots (`shot:<name>`)
# land in $UI_SHOTS (default target/ui-shots).
#
# In a container, the way CI would run it:
#
#   podman run --rm --userns=keep-id --security-opt label=disable \
#     -v "$PWD":/src -v "$HOME/.cargo/registry":"$HOME/.cargo/registry":ro -w /src \
#     localhost/matterfast-ui build-aux/ui-tests.sh --headless
#
# The registry mount matters for a debug build: the toolkit reads its icons
# from there at run time.
set -Eeuo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"

scenarios_dir=crates/aiddiction/tests/ui
app=${AIDDICTION_BIN:-target/debug/aiddiction}
timeout_s=${UI_TEST_TIMEOUT:-90}
shots=${UI_SHOTS:-$root/target/ui-shots}
mkdir -p -- "$shots"

headless=false
if [[ ${1:-} == --headless ]]; then
  shift
  headless=true
  runtime=$(mktemp -d)
  chmod 700 -- "$runtime"
  export XDG_RUNTIME_DIR=$runtime
  unset DISPLAY WAYLAND_DISPLAY
  printf '%s\n' 'default_border none' 'default_floating_border none' \
    'focus_follows_mouse no' > "$runtime/sway.conf"
  WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
    sway --unsupported-gpu -c "$runtime/sway.conf" >"$runtime/sway.log" 2>&1 &
  compositor_pid=$!
  for _ in $(seq 100); do
    socket=$(find "$runtime" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' -print -quit)
    [[ -n $socket ]] && break
    sleep 0.1
  done
  [[ -n ${socket:-} ]] || { cat "$runtime/sway.log" >&2; exit 1; }
  export WAYLAND_DISPLAY=${socket##*/}
  SWAYSOCK=$(find "$runtime" -maxdepth 1 -name 'sway-ipc.*' -print -quit)
  export SWAYSOCK
fi

[[ -x $app && -x target/debug/aiddiction-server && -x target/debug/aiddiction-fake-agent ]] \
  || cargo build -p aiddiction -p aiddiction-server -p aiddiction-fake-agent

if (( $# )); then
  files=()
  for name in "$@"; do files+=("$scenarios_dir/$name.script"); done
else
  files=("$scenarios_dir"/*.script)
fi

work=$(mktemp -d)
cleanup() {
  [[ -z ${compositor_pid:-} ]] || kill "$compositor_pid" 2>/dev/null || true
  rm -rf -- "$work" ${runtime:+"$runtime"}
}
trap cleanup EXIT

shot_cmd=$work/shot.sh
printf '#!/bin/sh\nexec grim "%s/$1.png"\n' "$shots" > "$shot_cmd"
chmod +x "$shot_cmd"

header() { sed -n "s/^# *$2: *//p" "$1" | head -n1; }

failed=()
for file in "${files[@]}"; do
  name=$(basename -- "$file" .script)
  [[ -f $file ]] || { echo "no such scenario: $name" >&2; exit 2; }
  size=$(header "$file" size)
  script=$(grep -v -E '^\s*(#|$)' "$file" | paste -sd ';')

  home=$work/$name
  mkdir -p -- "$home"
  $headless && swaymsg -q output HEADLESS-1 resolution "${size:-1100x760}"

  status=0
  env XDG_CONFIG_HOME="$home/config" XDG_CACHE_HOME="$home/cache" XDG_DATA_HOME="$home/data" \
    AIDDICTION_DEMO=1 AIDDICTION_COLOR=${AIDDICTION_COLOR:-#0b57d0} AIDDICTION_SIZE="${size:-1100x760}" AIDDICTION_SCRIPT="$script" \
    AIDDICTION_SHOT_CMD="$shot_cmd" FAKE_AGENT_DELAY_MS=${FAKE_AGENT_DELAY_MS:-20} \
    timeout "$timeout_s" "$app" >"$home/app.log" 2>&1 || status=$?

  if (( status == 0 )); then
    echo "ok    $name"
  else
    echo "FAIL  $name (status $status)"
    tail -n 30 "$home/app.log" | sed 's/^/      /'
    failed+=("$name")
  fi
done

(( ${#failed[@]} == 0 )) || { echo "${#failed[@]} failed: ${failed[*]}"; exit 1; }
