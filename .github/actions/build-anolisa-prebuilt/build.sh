#!/usr/bin/env bash
# Build and package one reproducible ANOLISA CLI precompiled release target.
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMON_DIR="$(cd "$ACTION_DIR/../prebuilt-rust-common" && pwd)"
FIXED_WORKTREE="${ANOLISA_SOURCE_WORKTREE:-/tmp/anolisa-raw-release-source-worktrees/anolisa}"
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
  --target-os {linux|macos} --target-arch {x86_64|aarch64} \
  --profile PROFILE [--tag anolisa/vX.Y.Z]
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
SEMVER_PATTERN='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.'
SEMVER_PATTERN+='(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?'
SEMVER_PATTERN+='(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'
[[ "$VERSION" =~ $SEMVER_PATTERN ]] || \
    die "invalid ANOLISA CLI version: $VERSION"

case "$TARGET_OS/$TARGET_ARCH/$PROFILE" in
    linux/x86_64/gnu2.17-x86_64)
        RUST_TARGET=x86_64-unknown-linux-gnu
        ;;
    linux/aarch64/gnu2.17-aarch64)
        RUST_TARGET=aarch64-unknown-linux-gnu
        ;;
    macos/aarch64/darwin11-aarch64)
        RUST_TARGET=aarch64-apple-darwin
        ;;
    *)
        die "profile $PROFILE does not match target $TARGET_OS/$TARGET_ARCH"
        ;;
esac

if [ -n "$TAG" ] && [ "$TAG" != "anolisa/v$VERSION" ]; then
    die "release tag $TAG does not match requested version $VERSION"
fi
for command in cargo cargo-sbom flock git gzip install python3 readelf sha256sum tar; do
    command -v "$command" >/dev/null || die "missing required command: $command"
done
SOURCE_REPO="$(git -C "$SOURCE_REPO" rev-parse --show-toplevel)" || \
    die "source-repo is not a Git worktree"
SOURCE_COMMIT="$(git -C "$SOURCE_REPO" rev-parse HEAD)"

if [ -n "$TAG" ]; then
    TAG_COMMIT="$(git -C "$SOURCE_REPO" rev-parse --verify "refs/tags/$TAG^{commit}")" || \
        die "release tag is unavailable in the checkout: $TAG"
    [ "$TAG_COMMIT" = "$SOURCE_COMMIT" ] || \
        die "release tag $TAG does not point at checked-out commit $SOURCE_COMMIT"
fi

install -d -m 0755 "$(dirname "$FIXED_WORKTREE")"
exec 9>"${FIXED_WORKTREE}.lock"
flock -n 9 || die "ANOLISA CLI source worktree is already in use"

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
git -C "$SOURCE_REPO" worktree lock --reason 'ANOLISA CLI prebuilt package build' \
    "$FIXED_WORKTREE"

COMPONENT_ROOT="$FIXED_WORKTREE/distribution/anolisa"
SOURCE_VERSION="$(
    python3 "$COMPONENT_ROOT/packaging/prebuilt/verify-release.py" \
        "$COMPONENT_ROOT" --os "$TARGET_OS" --arch "$TARGET_ARCH"
)"
[ "$VERSION" = "$SOURCE_VERSION" ] || \
    die "requested version $VERSION does not match ANOLISA CLI $SOURCE_VERSION"

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
    --manifest-path "$COMPONENT_ROOT/Cargo.toml" >/dev/null

(
    cd "$FIXED_WORKTREE"
    python3 "$COMMON_DIR/reproducible-build.py" \
        --source-root "$COMPONENT_ROOT" \
        --source-date-epoch "$SOURCE_DATE_EPOCH" \
        -- "$COMMON_DIR/cross-profile.sh" "$PROFILE" build \
        --release \
        --locked \
        --package anolisa-cli \
        --manifest-path distribution/anolisa/Cargo.toml
)

BIN_DIR="$COMPONENT_ROOT/target/$RUST_TARGET/release"
[ -x "$BIN_DIR/anolisa" ] || die "Cross build did not produce $BIN_DIR/anolisa"
if [ "$TARGET_OS" = macos ]; then
    python3 "$COMMON_DIR/verify-macho.py" --min 11.0 "$BIN_DIR/anolisa"
else
    python3 "$COMMON_DIR/verify-glibc.py" \
        --arch "$TARGET_ARCH" --max 2.17 "$BIN_DIR/anolisa"
fi

METADATA="$BIN_DIR/anolisa-build.toml"
cat > "$METADATA" <<EOF
version = "$VERSION"
target_os = "$TARGET_OS"
target_arch = "$TARGET_ARCH"

[binaries]
anolisa = "$(sha256sum "$BIN_DIR/anolisa" | awk '{print $1}')"
EOF

ANOLISA_SOURCE_DIR="$COMPONENT_ROOT" \
ANOLISA_BUILD_METADATA="$METADATA" \
BIN_DIR="$BIN_DIR" \
OUTPUT_DIR="$OUTPUT_DIR" \
TARGET_OS="$TARGET_OS" \
TARGET_ARCH="$TARGET_ARCH" \
SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
    "$COMPONENT_ROOT/packaging/prebuilt/package.sh" package >/dev/null

ARCHIVE="anolisa-$VERSION-$TARGET_OS-$TARGET_ARCH.tar.gz"
(
    cd "$OUTPUT_DIR"
    sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
)
python3 "$COMMON_DIR/generate-sbom.py" \
    --artifact "$OUTPUT_DIR/$ARCHIVE" \
    --component anolisa \
    --version "$VERSION" \
    --os "$TARGET_OS" \
    --arch "$TARGET_ARCH" \
    --target "$RUST_TARGET" \
    --project-dir "$COMPONENT_ROOT" \
    --source-date-epoch "$SOURCE_DATE_EPOCH" >/dev/null
python3 "$COMMON_DIR/verify-artifacts.py" \
    --directory "$OUTPUT_DIR" \
    --component anolisa \
    --version "$VERSION" \
    --layout flat \
    --os "$TARGET_OS" \
    --arch "$TARGET_ARCH"
printf 'Built ANOLISA CLI %s for %s/%s at %s\n' \
    "$VERSION" "$TARGET_OS" "$TARGET_ARCH" "$OUTPUT_DIR"
