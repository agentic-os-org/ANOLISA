#!/usr/bin/env bash
# Fetch the immutable RTK source required by current and historical Tokenless trees.
set -euo pipefail

RTK_REPOSITORY="https://github.com/rtk-ai/rtk.git"
TEMPORARY=""

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    if [ -n "$TEMPORARY" ] && [ -d "$TEMPORARY" ]; then
        rm -rf -- "$TEMPORARY"
    fi
}
trap cleanup EXIT

[ "$#" -eq 1 ] || die 'usage: setup-rtk.sh COMPONENT_ROOT'
COMPONENT_ROOT="$(cd "$1" && pwd)"
RTK_DIR="$COMPONENT_ROOT/third_party/rtk"

if [ -f "$COMPONENT_ROOT/scripts/setup-rtk.sh" ]; then
    bash "$COMPONENT_ROOT/scripts/setup-rtk.sh" "$RTK_DIR"
    exit 0
fi

RTK_RELEASE="$(
    sed -n 's/^rtk_tag[[:space:]]*:=[[:space:]]*"\(.*\)"/\1/p' \
        "$COMPONENT_ROOT/justfile"
)"
case "$RTK_RELEASE" in
    v0.43.0) RTK_COMMIT="5a7880d404db8364d602f2ecdc41dd790f64013f" ;;
    *) die "unsupported historical RTK release: ${RTK_RELEASE:-missing}" ;;
esac

[ ! -e "$RTK_DIR" ] || die "RTK destination already exists: $RTK_DIR"
install -d -m 0755 "$(dirname "$RTK_DIR")"
TEMPORARY="$(mktemp -d "${RTK_DIR}.tmp.XXXXXX")"
git init --quiet "$TEMPORARY"
git -C "$TEMPORARY" remote add origin "$RTK_REPOSITORY"
git -C "$TEMPORARY" fetch --quiet --depth 1 origin "$RTK_COMMIT"
git -C "$TEMPORARY" checkout --quiet --detach "$RTK_COMMIT"
[ "$(git -C "$TEMPORARY" rev-parse HEAD)" = "$RTK_COMMIT" ] || \
    die "fetched RTK does not match pinned commit $RTK_COMMIT"

patch --forward -p1 --no-backup-if-mismatch \
    -d "$TEMPORARY" < "$COMPONENT_ROOT/third_party/patches/rtk-tokenless-stats.patch"
patch --forward -p1 --no-backup-if-mismatch \
    -d "$TEMPORARY" < "$COMPONENT_ROOT/third_party/patches/rtk-pytest-error-report.patch"
printf '%s\n' "$RTK_COMMIT" > "$TEMPORARY/.anolisa-rtk-commit"

mv "$TEMPORARY" "$RTK_DIR"
TEMPORARY=""
printf 'RTK %s setup complete at %s\n' "$RTK_RELEASE" "$RTK_COMMIT"
