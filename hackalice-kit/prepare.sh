#!/bin/sh
# Create an independent source checkout; never install to a station.
set -eu
umask 077
KIT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
UPSTREAM=https://github.com/Morvex885/pulsar.git
FORK=https://github.com/Mostmac/pulsar.git
LEGACY_BASE=1a45067e30f9f9081252a65f87242e2a5659d25a
LEGACY=6385a0b1b0f5c61277cd181c14f06d803155cc33
LAB_BASE=37e2fe1c7e3438f05ebc7f8f1fc53030feb675e3

usage() {
    echo "Usage: sh prepare.sh legacy|wake-corr-lab NEW_DIRECTORY [upstream-ref]" >&2
    echo "An optional upstream-ref is supported only by legacy, for a new, untested port." >&2
    exit 2
}
fail() { printf '%s\n' "$*" >&2; exit 1; }
[ "$#" -ge 2 ] && [ "$#" -le 3 ] || usage
profile=$1
dest=$2
ref=${3:-}
case "$profile" in
    legacy) ;;
    wake-corr-lab) [ -z "$ref" ] || usage ;;
    *) usage ;;
esac
case "$ref" in -*) usage ;; esac
[ -n "$dest" ] || usage
[ ! -e "$dest" ] && [ ! -L "$dest" ] || fail "Destination already exists; choose a new directory."
command -v git >/dev/null || fail "git is required."
# Check diagnostic files before creating the destination.
check_hash() {
    expected=$1
    file=$2
    if command -v sha256sum >/dev/null; then
        actual=$(sha256sum "$file" | awk '{print $1}')
    elif command -v shasum >/dev/null; then
        actual=$(shasum -a 256 "$file" | awk '{print $1}')
    else
        fail "SHA-256 utility is required."
    fi
    [ "$actual" = "$expected" ] || fail "Invalid patch checksum: $file"
}
if [ "$profile" = wake-corr-lab ]; then
    check_hash 7791c1fd8aca134d9a56640509ece23a112372c5d3b01f7108ce9fc3b5937c3e "$KIT/patches/wake-levels-37e2fe1.patch"
    check_hash b31453d3a374e7e1b92dfb9fb17ce0e1d1056ad285424915729949177f4437f9 "$KIT/patches/wake-corr-37e2fe1.patch"
fi
# mkdir, rather than mkdir -p here, ensures we never touch an existing checkout.
mkdir -- "$dest"
dest=$(CDPATH= cd -- "$dest" && pwd -P)
tmp=$(mktemp -d)
ready=0
cleanup() {
    rc=$?
    trap - EXIT
    rm -rf -- "$tmp"
    if [ "$ready" = 0 ]; then
        printf 'Preparation incomplete. Separate checkout kept at %s. Do not build or deploy it.\n' "$dest" >&2
    fi
    exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
git -C "$dest" init -q
git -C "$dest" remote add upstream "$UPSTREAM"
git -C "$dest" remote add custom "$FORK"
g() { git -C "$dest" "$@"; }
fetch_exact() {
    g fetch --no-tags "$1" "$2"
    [ "$(g rev-parse FETCH_HEAD)" = "$2" ] || fail "Fetched revision differs from expected commit."
}
apply_checked() {
    g apply --check --index -- "$1"
    g apply --index -- "$1"
}
case "$profile" in
    legacy)
        fetch_exact custom "$LEGACY"
        g merge-base --is-ancestor "$LEGACY_BASE" "$LEGACY" || fail "Legacy history does not match."
        if [ -z "$ref" ]; then
            g checkout -q --detach "$LEGACY"
            base=$LEGACY
            status=legacy-source
        else
            # All committed legacy changes, not unpublished working-tree differences.
            g diff --binary "$LEGACY_BASE" "$LEGACY" > "$tmp/legacy.patch"
            [ -s "$tmp/legacy.patch" ] || fail "Legacy patch is empty."
            g fetch --no-tags upstream "$ref"
            base=$(g rev-parse FETCH_HEAD)
            g checkout -q --detach "$base"
            apply_checked "$tmp/legacy.patch"
            status=untested-port
        fi
        ;;
    wake-corr-lab)
        fetch_exact upstream "$LAB_BASE"
        g checkout -q --detach "$LAB_BASE"
        base=$LAB_BASE
        apply_checked "$KIT/patches/wake-levels-37e2fe1.patch"
        apply_checked "$KIT/patches/wake-corr-37e2fe1.patch"
        status=diagnostic-only
        ;;
esac
g diff --cached --check
g diff --check
# Mark only a fully prepared checkout. Store metadata inside .git, not source.
printf 'profile=%s\nbase=%s\nstatus=%s\nlegacy_commit=%s\n' \
    "$profile" "$base" "$status" "$LEGACY" > "$dest/.git/hackalice-kit-ready"
ready=1
printf 'PREPARED: %s\nprofile=%s\nbase=%s\nstatus=%s\n' "$dest" "$profile" "$base" "$status"
printf '%s\n' "No station or HA was modified. Review and test before installation."
