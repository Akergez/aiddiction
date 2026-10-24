#!/usr/bin/env bash
# Build Aidiction as a flatpak, and lay out the signed repository that
# app.akergez.Aidiction.flatpakref points at.
#
#   build-aux/publish-flatpak.sh             build this machine's architecture
#                                            into .flatpak-repo, nothing else
#   build-aux/publish-flatpak.sh --site DIR  sign .flatpak-repo and write DIR
#                                            as the static site that serves it
#
# The two halves are separate because CI builds each architecture on its own
# runner, one after the other into the same repository, and only then signs:
# the key is needed once, in the one job that publishes, instead of on every
# machine that compiles. The site is what GitHub Pages serves. It is laid out
# afresh on every publish, since Pages replaces the whole site anyway; a
# client then fetches the new commit whole instead of as a delta against the
# old one, which for one binary costs next to nothing.
#
# --site reads from the environment:
#   FLATPAK_REPO_URL     where DIR/repo will be reachable over HTTP      (required)
#   FLATPAK_GPG_ID       fingerprint or uid of the signing key           (required)
#   FLATPAK_GPG_KEY_B64  base64 of that key exported with --export-secret-keys,
#                        for CI; omit it to sign with the local keyring
#   FLATPAK_GPG_PASSPHRASE  that key's passphrase, required alongside it
#   FLATPAK_HOMEPAGE     the project page named in the ref file
#   FLATPAK_BUNDLE_DIR   also pack each architecture as a single-file
#                        aidiction-<version>-<arch>.flatpak into this directory
#   FLATPAK_BUNDLE_VERSION  the version in that name, required alongside it
set -Eeuo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$root"

app_id=app.akergez.Aidiction
app_title='Aidiction'
app_branch=${FLATPAK_BRANCH:-stable}

repo_dir=$root/.flatpak-repo
build_dir=$root/.flatpak-build
state_dir=$root/.flatpak-state

site=
case ${1:-} in
  --site) site=${2:?--site needs a directory} ;;
  '') ;;
  *) echo "usage: ${0##*/} [--site DIR]" >&2; exit 2 ;;
esac

if [[ -z $site ]]; then
  # flatpak build needs a session bus; a CI container has none of its own.
  [[ -n ${DBUS_SESSION_BUS_ADDRESS:-} ]] || exec dbus-run-session -- "$0" "$@"

  if command -v flatpak-builder >/dev/null; then
    builder=(flatpak-builder)
  else
    builder=(flatpak run org.flatpak.Builder)
  fi

  # rofiles-fuse cannot start inside a CI container: flatpak there believes it
  # is sandboxed and routes fusermount through a portal that is not running.
  "${builder[@]}" --force-clean --disable-rofiles-fuse \
    --state-dir="$state_dir" \
    --repo="$repo_dir" \
    --default-branch="$app_branch" \
    "$build_dir" "build-aux/$app_id.yml"

  echo "built into $repo_dir"
  exit 0
fi

[[ -d $repo_dir ]] || { echo "nothing to publish: $repo_dir does not exist" >&2; exit 1; }
[[ -n ${FLATPAK_REPO_URL:-} ]] || { echo 'set FLATPAK_REPO_URL' >&2; exit 1; }

# Some distributions ship GnuPG 1 as `gpg`; ostree needs 2.
gpg_bin=$(command -v gpg2 || command -v gpg) \
  || { echo 'gpg is required' >&2; exit 1; }

sign=()
if [[ -n ${FLATPAK_GPG_ID:-} ]]; then
  if [[ -n ${FLATPAK_GPG_KEY_B64:-} ]]; then
    umask 077
    # Outside the checkout: gpg-agent puts its sockets in the homedir when
    # there is no /run/user, and a socket has no business in a directory that
    # is about to be uploaded as an artifact.
    GNUPGHOME=$(mktemp -d)
    export GNUPGHOME
    trap 'rm -rf -- "$GNUPGHOME"' EXIT
    [[ -n ${FLATPAK_GPG_PASSPHRASE:-} ]] \
      || { echo 'FLATPAK_GPG_KEY_B64 needs FLATPAK_GPG_PASSPHRASE' >&2; exit 1; }
    printf '%s' "$FLATPAK_GPG_PASSPHRASE" > "$GNUPGHOME/passphrase"
    unset FLATPAK_GPG_PASSPHRASE
    chmod 600 -- "$GNUPGHOME/passphrase"
    # gpg-agent runs pinentry for every use of a secret key, and the build
    # image ships none at all. ostree signs through gpgme, which cannot pass a
    # passphrase itself, so the answer has to come from a pinentry.
    cat > "$GNUPGHOME/pinentry" <<'PINENTRY'
#!/bin/sh
echo "OK Pleased to meet you"
while IFS= read -r line; do
  case "$line" in
    GETPIN*) printf 'D %s\nOK\n' "$(cat "${0%/*}/passphrase")" ;;
    BYE*)    echo "OK closing connection"; exit 0 ;;
    *)       echo "OK" ;;
  esac
done
PINENTRY
    chmod 700 -- "$GNUPGHOME/pinentry"
    printf 'pinentry-program %s/pinentry\nallow-loopback-pinentry\n' \
      "$GNUPGHOME" > "$GNUPGHOME/gpg-agent.conf"
    key=$FLATPAK_GPG_KEY_B64
    unset FLATPAK_GPG_KEY_B64
    # --ignore-garbage: a value pasted into a CI variable from a terminal
    # arrives with whatever the terminal added — wrapped lines, spaces, the
    # shell's end-of-output mark. A key that is really damaged still fails,
    # one step later, at the import.
    printf '%s' "$key" | base64 -d --ignore-garbage | "$gpg_bin" --batch --quiet \
      --pinentry-mode loopback --passphrase-file "$GNUPGHOME/passphrase" --import
    unset key
    sign=(--gpg-sign="$FLATPAK_GPG_ID" --gpg-homedir="$GNUPGHOME")
  else
    sign=(--gpg-sign="$FLATPAK_GPG_ID")
  fi
fi

if (( ${#sign[@]} == 0 )); then
  echo 'refusing to publish an unsigned repository: set FLATPAK_GPG_ID' >&2
  exit 1
fi

# Signing happens here rather than inside flatpak-builder: locally the builder
# is itself a flatpak, and the gpg-agent it starts in that sandbox has no
# pinentry to fall back on.
flatpak build-sign "${sign[@]}" "$repo_dir"

flatpak build-update-repo \
  --title="$app_title" \
  --default-branch="$app_branch" \
  --generate-static-deltas \
  --prune \
  "${sign[@]}" \
  "$repo_dir"

mkdir -p -- "$site"
rm -rf -- "$site/repo"
cp -a -- "$repo_dir" "$site/repo"
# ostree's lock and scratch space are runtime state, not published content.
rm -rf -- "$site/repo/.lock" "$site/repo/tmp"

# The ref file is published beside the repository it points at, so its
# embedded key can never drift from the key that signed the commit, and the
# source tree never has to carry a copy.
key_b64=$("$gpg_bin" --export "$FLATPAK_GPG_ID" | base64 -w0)
{
  printf '%s\n' '[Flatpak Ref]' "Title=$app_title" "Name=$app_id" \
    "Branch=$app_branch" "Url=$FLATPAK_REPO_URL"
  [[ -z ${FLATPAK_HOMEPAGE:-} ]] || printf 'Homepage=%s\n' "$FLATPAK_HOMEPAGE"
  printf '%s\n' 'RuntimeRepo=https://flathub.org/repo/flathub.flatpakrepo' \
    'IsRuntime=false' "GPGKey=$key_b64"
} > "$site/$app_id.flatpakref"

if [[ -n ${FLATPAK_BUNDLE_DIR:-} ]]; then
  bundle_version=${FLATPAK_BUNDLE_VERSION:?FLATPAK_BUNDLE_DIR needs FLATPAK_BUNDLE_VERSION}
  mkdir -p -- "$FLATPAK_BUNDLE_DIR"
  # One bundle per architecture the repository holds.
  for ref in "$repo_dir/refs/heads/app/$app_id"/*/; do
    bundle_arch=$(basename -- "$ref")
    bundle=$FLATPAK_BUNDLE_DIR/aidiction-$bundle_version-$bundle_arch.flatpak
    flatpak build-bundle --arch="$bundle_arch" \
      --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo \
      "$repo_dir" "$bundle" "$app_id" "$app_branch"
    (cd -- "$FLATPAK_BUNDLE_DIR" && sha256sum "${bundle##*/}" > "${bundle##*/}.sha256")
  done
fi

echo "site written to $site"
echo "install with: ${FLATPAK_REPO_URL%/repo}/$app_id.flatpakref"
