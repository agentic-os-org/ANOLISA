#!/usr/bin/env bash
# Fetch and patch the immutable RTK source used by Tokenless builds.
set -euo pipefail

COMPONENT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RTK_REPOSITORY="https://github.com/rtk-ai/rtk.git"
RTK_RELEASE="v0.49.0"
RTK_COMMIT="b1c0dc00649c50fbe8930f849c800d4d6ca12091"
RTK_DIR="${1:-$COMPONENT_ROOT/third_party/rtk}"
PATCH_DIR="$COMPONENT_ROOT/third_party/patches"
REVISION_MARKER="$RTK_DIR/.anolisa-rtk-commit"
PATCHES=(rtk-tokenless-stats.patch rtk-pytest-error-report.patch rtk-library.patch)
PATCH_MARKER="$RTK_DIR/.anolisa-rtk-patches"
TREE_MARKER="$RTK_DIR/.anolisa-rtk-tree"
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

[ "$#" -le 1 ] || die 'usage: setup-rtk.sh [destination]'

patch_digest() {
    for name in "${PATCHES[@]}"; do
        printf '%s\0' "$name"
        cat "$PATCH_DIR/$name"
    done | {
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum
        else
            shasum -a 256
        fi
    } | cut -d ' ' -f 1
}
# Hash the applied source, including untracked additions and symlink targets.
# Cargo output and setup markers are excluded so clean offline builds can reuse it.
tree_digest() {
    (
        cd "$1"
        find . \( -path './.git' -o -path './target' -o -path './.anolisa-rtk-*' \) -prune -o \( -type f -o -type l \) -print |
            LC_ALL=C sort | while IFS= read -r path; do
                printf '%s\0' "$path"
                if [ -L "$path" ]; then
                    readlink "$path"
                elif command -v sha256sum >/dev/null 2>&1; then
                    sha256sum "$path"
                else
                    shasum -a 256 "$path"
                fi
            done
    ) | {
        if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi
    } | cut -d ' ' -f 1
}
PATCH_DIGEST="$(patch_digest)"

if [ -e "$RTK_DIR" ]; then
    [ -f "$RTK_DIR/Cargo.toml" ] || \
        die "RTK destination is incomplete: $RTK_DIR"
    [ -f "$REVISION_MARKER" ] || \
        die "RTK revision marker is missing; run just clean-rtk and retry"
    [ "$(cat "$REVISION_MARKER")" = "$RTK_COMMIT" ] || \
        die "RTK checkout does not match pinned commit $RTK_COMMIT; run just clean-rtk and retry"
    [ -f "$PATCH_MARKER" ] && [ "$(cat "$PATCH_MARKER")" = "$PATCH_DIGEST" ] || \
        die "RTK patches have changed; run just clean-rtk and retry"
    if [ -d "$RTK_DIR/.git" ]; then
        [ "$(git -C "$RTK_DIR" rev-parse HEAD)" = "$RTK_COMMIT" ] || \
            die "RTK HEAD does not match pinned commit $RTK_COMMIT"
    fi
    [ -f "$TREE_MARKER" ] && [ "$(cat "$TREE_MARKER")" = "$(tree_digest "$RTK_DIR")" ] || \
        die "RTK applied source has changed or its digest is missing; preserve edits, run just clean-rtk and retry"
    printf 'RTK %s is already set up at %s\n' "$RTK_RELEASE" "$RTK_COMMIT"
    exit 0
fi

install -d -m 0755 "$(dirname "$RTK_DIR")"
TEMPORARY="$(mktemp -d "${RTK_DIR}.tmp.XXXXXX")"
git init --quiet "$TEMPORARY"
git -C "$TEMPORARY" remote add origin "$RTK_REPOSITORY"
git -C "$TEMPORARY" fetch --quiet --depth 1 origin "$RTK_COMMIT" || \
    die "Cannot fetch pinned RTK; allow network access to $RTK_REPOSITORY or provide a previously verified checkout"
git -C "$TEMPORARY" checkout --quiet --detach "$RTK_COMMIT"
[ "$(git -C "$TEMPORARY" rev-parse HEAD)" = "$RTK_COMMIT" ] || \
    die "fetched RTK does not match pinned commit $RTK_COMMIT"

for name in "${PATCHES[@]}"; do
    patch --forward -p1 --no-backup-if-mismatch \
        -d "$TEMPORARY" < "$PATCH_DIR/$name"
done
printf '%s\n' "$RTK_COMMIT" > "$TEMPORARY/.anolisa-rtk-commit"
printf '%s\n' "$PATCH_DIGEST" > "$TEMPORARY/.anolisa-rtk-patches"
tree_digest "$TEMPORARY" > "$TEMPORARY/.anolisa-rtk-tree"

mv "$TEMPORARY" "$RTK_DIR"
TEMPORARY=""
printf 'RTK %s setup complete at %s\n' "$RTK_RELEASE" "$RTK_COMMIT"
