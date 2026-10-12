#!/usr/bin/env bash
# Build and package one reproducible ws-ckpt precompiled release target.
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMON_DIR="$(cd "$ACTION_DIR/../prebuilt-rust-common" && pwd)"
FIXED_WORKTREE="${WS_CKPT_SOURCE_WORKTREE:-/tmp/anolisa-raw-release-source-worktrees/ws-ckpt}"
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

usage() {
    cat <<'EOF'
Usage: build.sh --source-repo PATH --output-dir PATH --version X.Y.Z \
  --target-os linux --target-arch {x86_64|aarch64} \
  --profile PROFILE [--tag ckpt/vX.Y.Z]
EOF
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
        --help | -h) usage; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done
[ -n "$VERSION" ] || die '--version is required'
[ -n "$TARGET_OS" ] || die '--target-os is required'
[ -n "$TARGET_ARCH" ] || die '--target-arch is required'
[ -n "$PROFILE" ] || die '--profile is required'
[ -n "$SOURCE_REPO" ] || die '--source-repo is required'
[ -n "$OUTPUT_DIR" ] || die '--output-dir is required'

for command in cargo flock git install python3 sha256sum; do
    command -v "$command" >/dev/null || die "missing required command: $command"
done
SOURCE_REPO="$(git -C "$SOURCE_REPO" rev-parse --show-toplevel)" || \
    die "source-repo is not a Git worktree"
SOURCE_COMMIT="$(git -C "$SOURCE_REPO" rev-parse HEAD)"

case "$TARGET_OS/$TARGET_ARCH/$PROFILE" in
    linux/x86_64/gnu2.28-x86_64)
        RUST_TARGET=x86_64-unknown-linux-gnu
        ;;
    linux/aarch64/gnu2.28-aarch64)
        RUST_TARGET=aarch64-unknown-linux-gnu
        ;;
    *)
        die "profile $PROFILE does not match target $TARGET_OS/$TARGET_ARCH"
        ;;
esac

if [ -n "$TAG" ]; then
    [ "$TAG" = "ckpt/v$VERSION" ] || \
        die "release tag $TAG does not match requested version $VERSION"
    TAG_COMMIT="$(git -C "$SOURCE_REPO" rev-parse --verify "refs/tags/$TAG^{commit}")" || \
        die "release tag is unavailable in the checkout: $TAG"
    [ "$TAG_COMMIT" = "$SOURCE_COMMIT" ] || \
        die "release tag $TAG does not point at checked-out commit $SOURCE_COMMIT"
fi

install -d -m 0755 "$(dirname "$FIXED_WORKTREE")"
exec 9>"${FIXED_WORKTREE}.lock"
flock -n 9 || die "ws-ckpt source worktree is already in use"

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

[ ! -L "$FIXED_WORKTREE" ] || die "fixed worktree path must not be a symbolic link"
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
git -C "$SOURCE_REPO" worktree lock --reason 'ws-ckpt prebuilt package build' \
    "$FIXED_WORKTREE"

COMPONENT_ROOT="$FIXED_WORKTREE/src/ws-ckpt"
(
    cd "$COMPONENT_ROOT/src/plugins/openclaw"
    npm ci --ignore-scripts --omit=peer
    npm run build
)
COMPONENT_VERSION="$(
    python3 "$COMPONENT_ROOT/packaging/raw/verify-release.py" \
        "$COMPONENT_ROOT" "$COMPONENT_ROOT/.anolisa/component.toml" \
        --os "$TARGET_OS" --arch "$TARGET_ARCH"
)"
[ "$VERSION" = "$COMPONENT_VERSION" ] || \
    die "requested version $VERSION does not match ws-ckpt $COMPONENT_VERSION"

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

cargo metadata \
    --format-version 1 \
    --locked \
    --filter-platform "$RUST_TARGET" \
    --manifest-path "$COMPONENT_ROOT/src/Cargo.toml" >/dev/null

(
    cd "$FIXED_WORKTREE"
    python3 "$COMMON_DIR/reproducible-build.py" \
        --source-root "$COMPONENT_ROOT" \
        --source-date-epoch "$SOURCE_DATE_EPOCH" \
        -- "$COMMON_DIR/cross-profile.sh" "$PROFILE" build \
        --release \
        --workspace \
        --locked \
        --manifest-path src/ws-ckpt/src/Cargo.toml
)

BIN_DIR="$COMPONENT_ROOT/src/target/release"
TARGET_BIN_DIR="$COMPONENT_ROOT/src/target/$RUST_TARGET/release"
install -d -m 0755 "$BIN_DIR"
[ -x "$TARGET_BIN_DIR/ws-ckpt" ] || \
    die "Cross build did not produce $TARGET_BIN_DIR/ws-ckpt"
install -p -m 0755 "$TARGET_BIN_DIR/ws-ckpt" "$BIN_DIR/ws-ckpt"
python3 "$COMMON_DIR/verify-glibc.py" \
    --arch "$TARGET_ARCH" --max 2.28 "$BIN_DIR/ws-ckpt"

METADATA="$BIN_DIR/ws-ckpt-build.toml"
{
    printf 'version = "%s"\n' "$VERSION"
    printf 'target_os = "%s"\n' "$TARGET_OS"
    printf 'target_arch = "%s"\n\n' "$TARGET_ARCH"
    printf '[binaries]\n'
    printf 'ws-ckpt = "%s"\n' "$(sha256sum "$BIN_DIR/ws-ckpt" | awk '{print $1}')"
} > "$METADATA"

CONTRACT="$COMPONENT_ROOT/.anolisa/component.toml"
WS_CKPT_SOURCE_DIR="$COMPONENT_ROOT" \
RAW_CONTRACT="$CONTRACT" \
BIN_DIR="$BIN_DIR" \
WS_CKPT_BUILD_METADATA="$METADATA" \
OUTPUT_DIR="$OUTPUT_DIR" \
TARGET_OS="$TARGET_OS" \
TARGET_ARCH="$TARGET_ARCH" \
SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
    "$COMPONENT_ROOT/packaging/raw/package.sh" package >/dev/null

ARCHIVE="ws-ckpt-$VERSION-$TARGET_OS-$TARGET_ARCH.tar.gz"
(
    cd "$OUTPUT_DIR"
    sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
)
python3 "$COMMON_DIR/generate-sbom.py" \
    --artifact "$OUTPUT_DIR/$ARCHIVE" \
    --component ws-ckpt \
    --version "$VERSION" \
    --os "$TARGET_OS" \
    --arch "$TARGET_ARCH" \
    --target "$RUST_TARGET" \
    --project-dir "$COMPONENT_ROOT/src" \
    --source-date-epoch "$SOURCE_DATE_EPOCH" >/dev/null
python3 "$COMMON_DIR/verify-artifacts.py" \
    --directory "$OUTPUT_DIR" \
    --component ws-ckpt \
    --version "$VERSION" \
    --layout flat \
    --os "$TARGET_OS" \
    --arch "$TARGET_ARCH"
printf 'Built ws-ckpt %s for %s/%s at %s\n' \
    "$VERSION" "$TARGET_OS" "$TARGET_ARCH" "$OUTPUT_DIR"
