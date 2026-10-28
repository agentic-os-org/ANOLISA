#!/usr/bin/env bash
# Fetch and patch the immutable RTK source used by Tokenless builds.
set -euo pipefail

COMPONENT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RTK_REPOSITORY="https://github.com/rtk-ai/rtk.git"
RTK_RELEASE="v0.49.0"
RTK_COMMIT="b1c0dc00649c50fbe8930f849c800d4d6ca12091"
RTK_DIR="${1:-$COMPONENT_ROOT/third_party/rtk}"
PATCH_DIR="$COMPONENT_ROOT/third_party/patches"
# Applied in order. build-windows.ps1 and tests/test_windows.py parse this
# list too: keep one bare file name per line, with no quotes or comments,
# and the closing parenthesis alone at the start of a line.
PATCHES=(
    rtk-tokenless-stats.patch
    rtk-pytest-error-report.patch
)
REVISION_MARKER="$RTK_DIR/.anolisa-rtk-commit"
PATCH_MARKER="$RTK_DIR/.anolisa-rtk-patches"
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

# Identifies the patch set, so a checkout patched from a different set is
# rejected instead of silently building stale sources. Computed up front so a
# missing sha256sum or patch file is reported as such, not as a mismatch.
PATCH_DIGEST="$(cd "$PATCH_DIR" && sha256sum -- "${PATCHES[@]}")" || \
    die "cannot fingerprint the RTK patches in $PATCH_DIR"

if [ -e "$RTK_DIR" ]; then
    [ -f "$RTK_DIR/Cargo.toml" ] || \
        die "RTK destination is incomplete: $RTK_DIR"
    [ -f "$REVISION_MARKER" ] || \
        die "RTK revision marker is missing; run just clean-rtk and retry"
    [ "$(cat "$REVISION_MARKER")" = "$RTK_COMMIT" ] || \
        die "RTK checkout does not match pinned commit $RTK_COMMIT; run just clean-rtk and retry"
    if [ -d "$RTK_DIR/.git" ]; then
        [ "$(git -C "$RTK_DIR" rev-parse HEAD)" = "$RTK_COMMIT" ] || \
            die "RTK HEAD does not match pinned commit $RTK_COMMIT"
    fi
    [ -f "$PATCH_MARKER" ] || \
        die "RTK patch-set marker is missing from $RTK_DIR; run just clean-rtk and retry"
    [ "$(cat "$PATCH_MARKER")" = "$PATCH_DIGEST" ] || \
        die "RTK checkout at $RTK_DIR was patched with a different patch set; run just clean-rtk and retry"
    printf 'RTK %s is already set up at %s\n' "$RTK_RELEASE" "$RTK_COMMIT"
    exit 0
fi

install -d -m 0755 "$(dirname "$RTK_DIR")"
TEMPORARY="$(mktemp -d "${RTK_DIR}.tmp.XXXXXX")"
git init --quiet "$TEMPORARY"
git -C "$TEMPORARY" remote add origin "$RTK_REPOSITORY"
git -C "$TEMPORARY" fetch --quiet --depth 1 origin "$RTK_COMMIT"
git -C "$TEMPORARY" checkout --quiet --detach "$RTK_COMMIT"
[ "$(git -C "$TEMPORARY" rev-parse HEAD)" = "$RTK_COMMIT" ] || \
    die "fetched RTK does not match pinned commit $RTK_COMMIT"

for name in "${PATCHES[@]}"; do
    patch --forward -p1 --no-backup-if-mismatch \
        -d "$TEMPORARY" < "$PATCH_DIR/$name"
done
printf '%s\n' "$RTK_COMMIT" > "$TEMPORARY/.anolisa-rtk-commit"
printf '%s\n' "$PATCH_DIGEST" > "$TEMPORARY/.anolisa-rtk-patches"

mv "$TEMPORARY" "$RTK_DIR"
TEMPORARY=""
printf 'RTK %s setup complete at %s\n' "$RTK_RELEASE" "$RTK_COMMIT"
