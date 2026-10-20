#!/usr/bin/env bash
# Build one sec-core V2 raw package from the tagged source revision.
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMON_DIR="$(cd "$ACTION_DIR/../prebuilt-rust-common" && pwd)"
FIXED_WORKTREE="${SEC_CORE_SOURCE_WORKTREE:-/tmp/anolisa-raw-release-source-worktrees/sec-core}"
VERSION=""
TARGET_OS=""
TARGET_ARCH=""
PROFILE=""
TAG=""
SOURCE_REPO=""
OUTPUT_DIR=""
WORKTREE_READY=0

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

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
[ -n "$VERSION" ] || die '--version is required'
[ -n "$SOURCE_REPO" ] || die '--source-repo is required'
[ -n "$OUTPUT_DIR" ] || die '--output-dir is required'
[ "$TARGET_OS/$TARGET_ARCH/$PROFILE" = linux/x86_64/gnu2.28-x86_64 ] || \
    die "profile $PROFILE does not match target $TARGET_OS/$TARGET_ARCH"

for command in cargo flock git install npm python3 sha256sum; do
    command -v "$command" >/dev/null || die "missing required command: $command"
done
SOURCE_REPO="$(git -C "$SOURCE_REPO" rev-parse --show-toplevel)" || \
    die 'source-repo is not a Git worktree'
SOURCE_COMMIT="$(git -C "$SOURCE_REPO" rev-parse HEAD)"
if [ -n "$TAG" ]; then
    [ "$TAG" = "sec-core/v$VERSION" ] || \
        die "release tag $TAG does not match requested version $VERSION"
    TAG_COMMIT="$(git -C "$SOURCE_REPO" rev-parse --verify "refs/tags/$TAG^{commit}")" || \
        die "release tag is unavailable in the checkout: $TAG"
    [ "$TAG_COMMIT" = "$SOURCE_COMMIT" ] || \
        die "release tag $TAG does not point at checked-out commit $SOURCE_COMMIT"
fi

install -d -m 0755 "$(dirname "$FIXED_WORKTREE")"
exec 9>"${FIXED_WORKTREE}.lock"
flock -n 9 || die 'sec-core source worktree is already in use'

worktree_registered() {
    git -C "$SOURCE_REPO" worktree list --porcelain | \
        grep -Fxq "worktree $FIXED_WORKTREE"
}

cleanup() {
    if [ "$WORKTREE_READY" -eq 1 ] && worktree_registered; then
        git -C "$SOURCE_REPO" worktree unlock "$FIXED_WORKTREE" >/dev/null 2>&1 || true
        git -C "$SOURCE_REPO" worktree remove --force "$FIXED_WORKTREE" >/dev/null 2>&1 || \
            printf 'WARN: failed to remove source worktree: %s\n' "$FIXED_WORKTREE" >&2
    fi
}
trap cleanup EXIT

[ ! -L "$FIXED_WORKTREE" ] || die 'fixed worktree path must not be a symbolic link'
if worktree_registered; then
    git -C "$SOURCE_REPO" worktree unlock "$FIXED_WORKTREE" >/dev/null 2>&1 || true
    git -C "$SOURCE_REPO" worktree remove --force "$FIXED_WORKTREE" || \
        die "cannot remove the previous registered worktree: $FIXED_WORKTREE"
elif [ -e "$FIXED_WORKTREE" ]; then
    [ -d "$FIXED_WORKTREE" ] || die "fixed worktree path is not a directory"
    rmdir "$FIXED_WORKTREE" 2>/dev/null || \
        die "refusing to delete a non-empty unregistered path: $FIXED_WORKTREE"
fi
git -C "$SOURCE_REPO" worktree add --detach "$FIXED_WORKTREE" "$SOURCE_COMMIT"
WORKTREE_READY=1
git -C "$SOURCE_REPO" worktree lock --reason 'sec-core prebuilt package build' \
    "$FIXED_WORKTREE"

COMPONENT_ROOT="$FIXED_WORKTREE/src/agent-sec-core"
COMPONENT_VERSION="$(python3 "$COMPONENT_ROOT/packaging/raw/verify_release.py" \
    "$COMPONENT_ROOT" "$COMPONENT_ROOT/.anolisa/component.toml")"
[ "$VERSION" = "$COMPONENT_VERSION" ] || \
    die "requested version $VERSION does not match sec-core $COMPONENT_VERSION"
if [ -e "$OUTPUT_DIR" ] && [ -n "$(find "$OUTPUT_DIR" -mindepth 1 -print -quit)" ]; then
    die "output directory must be empty: $OUTPUT_DIR"
fi
install -d -m 0755 "$OUTPUT_DIR"

SOURCE_DATE_EPOCH="$(git -C "$FIXED_WORKTREE" show -s --format=%ct HEAD)"
case "$SOURCE_DATE_EPOCH" in
    '' | *[!0-9]*) die 'source commit timestamp is not numeric' ;;
esac
export RUSTUP_TOOLCHAIN=1.93.0-x86_64-unknown-linux-gnu
export SOURCE_DATE_EPOCH
RUST_TARGET=x86_64-unknown-linux-gnu
for project in v2 linux-sandbox; do
    cargo metadata --format-version 1 --locked --filter-platform "$RUST_TARGET" \
        --manifest-path "$COMPONENT_ROOT/$project/Cargo.toml" >/dev/null
done

python3 "$COMMON_DIR/reproducible-build.py" \
    --source-root "$COMPONENT_ROOT" \
    --source-date-epoch "$SOURCE_DATE_EPOCH" \
    -- make -C "$COMPONENT_ROOT" package-raw \
        OUTPUT_DIR="$OUTPUT_DIR" \
        TARGET_OS="$TARGET_OS" TARGET_ARCH="$TARGET_ARCH" \
        RAW_RUST_TARGET="$RUST_TARGET" \
        RAW_RUST_BUILDER="$COMMON_DIR/cross-profile.sh $PROFILE" \
        NPM_INSTALL='npm ci --include=dev'

BUILD_DIR="$COMPONENT_ROOT/target"
for binary in agent-sec-cli agent-sec-daemon agent-sec-daemon-health; do
    python3 "$COMMON_DIR/verify-glibc.py" --arch "$TARGET_ARCH" --max 2.28 \
        "$BUILD_DIR/v2/bin/$binary"
done
python3 "$COMMON_DIR/verify-glibc.py" --arch "$TARGET_ARCH" --max 2.28 \
    "$BUILD_DIR/linux-sandbox"
python3 "$COMMON_DIR/verify-glibc.py" --arch "$TARGET_ARCH" --max 2.28 \
    "$BUILD_DIR/python-runtime/bin/python3.11"

ARCHIVE="sec-core-$VERSION-$TARGET_OS-$TARGET_ARCH.tar.gz"
FIRST_SHA="$(sha256sum "$OUTPUT_DIR/$ARCHIVE" | awk '{print $1}')"
(
    cd "$COMPONENT_ROOT"
    BUILD_DIR="$BUILD_DIR" OUTPUT_DIR="$OUTPUT_DIR" \
        TARGET_OS="$TARGET_OS" TARGET_ARCH="$TARGET_ARCH" \
        "$COMPONENT_ROOT/packaging/raw/package.sh" package >/dev/null
)
[ "$(sha256sum "$OUTPUT_DIR/$ARCHIVE" | awk '{print $1}')" = "$FIRST_SHA" ] || \
    die 'repeated packaging changed the archive'
(
    cd "$OUTPUT_DIR"
    sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
)
PYTHON_SBOM="$BUILD_DIR/python-runtime.sbom.json"
python3 "$ACTION_DIR/python-sbom.py" "$BUILD_DIR/python-runtime" "$PYTHON_SBOM"
python3 "$COMMON_DIR/generate-sbom.py" \
    --artifact "$OUTPUT_DIR/$ARCHIVE" \
    --component sec-core --version "$VERSION" \
    --os "$TARGET_OS" --arch "$TARGET_ARCH" --target "$RUST_TARGET" \
    --project-dir "$COMPONENT_ROOT/v2" \
    --project-dir "$COMPONENT_ROOT/linux-sandbox" \
    --extra-sbom "$PYTHON_SBOM" \
    --source-date-epoch "$SOURCE_DATE_EPOCH" >/dev/null
python3 "$COMMON_DIR/verify-artifacts.py" \
    --directory "$OUTPUT_DIR" --component sec-core --version "$VERSION" \
    --layout flat --os "$TARGET_OS" --arch "$TARGET_ARCH"
printf 'Built sec-core %s for %s/%s at %s\n' \
    "$VERSION" "$TARGET_OS" "$TARGET_ARCH" "$OUTPUT_DIR"
