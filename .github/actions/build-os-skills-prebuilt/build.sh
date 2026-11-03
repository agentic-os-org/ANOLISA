#!/usr/bin/env bash
# Package static OS Skills from the exact checkout revision.
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMON_DIR="$(cd "$ACTION_DIR/../prebuilt-rust-common" && pwd)"
WORKTREE="${OS_SKILLS_SOURCE_WORKTREE:-/tmp/anolisa-raw-release-source-worktrees/os-skills}"
SOURCE_REPO=""
OUTPUT_DIR=""
VERSION=""
TARGET_OS=""
TARGET_ARCH=""
PROFILE=""
TAG=""
WORKTREE_READY=0

die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

while [ "$#" -gt 0 ]; do
    case "$1" in
        --source-repo) SOURCE_REPO="${2:-}"; shift 2 ;;
        --output-dir) OUTPUT_DIR="${2:-}"; shift 2 ;;
        --version) VERSION="${2:-}"; shift 2 ;;
        --target-os) TARGET_OS="${2:-}"; shift 2 ;;
        --target-arch) TARGET_ARCH="${2:-}"; shift 2 ;;
        --profile) PROFILE="${2:-}"; shift 2 ;;
        --tag) TAG="${2:-}"; shift 2 ;;
        *) die "unknown argument: $1" ;;
    esac
done
for required in SOURCE_REPO OUTPUT_DIR VERSION TARGET_OS TARGET_ARCH PROFILE; do
    [ -n "${!required}" ] || die "missing required argument: $required"
done
[ "$TARGET_OS" = linux ] && [ "$PROFILE" = static-linux ] || \
    die "profile $PROFILE does not match $TARGET_OS"
case "$TARGET_ARCH" in x86_64 | aarch64) ;; *) die "unsupported target: $TARGET_ARCH" ;; esac
SOURCE_REPO="$(git -C "$SOURCE_REPO" rev-parse --show-toplevel)"
SOURCE_COMMIT="$(git -C "$SOURCE_REPO" rev-parse HEAD)"
if [ -n "$TAG" ]; then
    [ "$TAG" = "skill/v$VERSION" ] || die "release tag does not match version: $TAG"
    TAG_COMMIT="$(git -C "$SOURCE_REPO" rev-parse --verify "refs/tags/$TAG^{commit}")" || \
        die "release tag is unavailable: $TAG"
    [ "$TAG_COMMIT" = "$SOURCE_COMMIT" ] || die "release tag does not point at source commit"
fi

install -d -m 0755 "$(dirname "$WORKTREE")"
exec 9>"${WORKTREE}.lock"
flock -n 9 || die "OS Skills source worktree is already in use"
registered() {
    git -C "$SOURCE_REPO" worktree list --porcelain | grep -Fxq "worktree $WORKTREE"
}
cleanup() {
    if [ "$WORKTREE_READY" -eq 1 ] && registered; then
        git -C "$SOURCE_REPO" worktree unlock "$WORKTREE" >/dev/null 2>&1 || true
        git -C "$SOURCE_REPO" worktree remove --force "$WORKTREE" >/dev/null 2>&1 || \
            printf 'WARN: failed to remove source worktree: %s\n' "$WORKTREE" >&2
    fi
}
trap cleanup EXIT
[ ! -L "$WORKTREE" ] || die "source worktree path is a symlink"
if registered; then
    git -C "$SOURCE_REPO" worktree unlock "$WORKTREE" >/dev/null 2>&1 || true
    git -C "$SOURCE_REPO" worktree remove --force "$WORKTREE"
elif [ -e "$WORKTREE" ]; then
    rmdir "$WORKTREE" || die "refusing to replace non-empty unregistered worktree"
fi
git -C "$SOURCE_REPO" worktree add --detach "$WORKTREE" "$SOURCE_COMMIT"
WORKTREE_READY=1
git -C "$SOURCE_REPO" worktree lock --reason 'os-skills raw package build' "$WORKTREE"
SOURCE="$WORKTREE/src/os-skills"
ACTUAL_VERSION="$(python3 - "$SOURCE/component.toml" <<'PY'
import sys, tomllib
with open(sys.argv[1], 'rb') as stream:
    print(tomllib.load(stream)['component']['version'])
PY
)"
[ "$VERSION" = "$ACTUAL_VERSION" ] || die "requested version differs from component.toml"
if [ -e "$OUTPUT_DIR" ] && [ -n "$(find "$OUTPUT_DIR" -mindepth 1 -print -quit)" ]; then
    die "output directory must be empty"
fi
EPOCH="$(git -C "$WORKTREE" show -s --format=%ct HEAD)"
printf 'Source commit: %s\n' "$SOURCE_COMMIT"
for attempt in 1 2; do
    python3 "$SOURCE/packaging/raw/package.py" --source-dir "$SOURCE" \
        --output-dir "$OUTPUT_DIR" --version "$VERSION" \
        --target-arch "$TARGET_ARCH" --epoch "$EPOCH"
    archive="$OUTPUT_DIR/os-skills-$VERSION-linux-$TARGET_ARCH.tar.gz"
    digest="$(sha256sum "$archive" | awk '{print $1}')"
    if [ "$attempt" = 1 ]; then first_digest="$digest"
    else [ "$digest" = "$first_digest" ] || die 'repeated packaging changed the archive'; fi
done
python3 "$COMMON_DIR/verify-artifacts.py" --directory "$OUTPUT_DIR" \
    --component os-skills --version "$VERSION" --layout flat \
    --os linux --arch "$TARGET_ARCH"
printf 'Built OS Skills %s for linux/%s at %s\n' "$VERSION" "$TARGET_ARCH" "$OUTPUT_DIR"
