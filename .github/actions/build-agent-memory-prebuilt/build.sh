#!/usr/bin/env bash
# Build and package one reproducible agent-memory precompiled release target.
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMON_DIR="$(cd "$ACTION_DIR/../prebuilt-rust-common" && pwd)"
FIXED_WORKTREE="${AGENT_MEMORY_SOURCE_WORKTREE:-/tmp/anolisa-raw-release-source-worktrees/agent-memory}"
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
  --profile PROFILE [--tag memory/vX.Y.Z]
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
    [ "$TAG" = "memory/v$VERSION" ] || \
        die "release tag $TAG does not match requested version $VERSION"
    TAG_COMMIT="$(git -C "$SOURCE_REPO" rev-parse --verify "refs/tags/$TAG^{commit}")" || \
        die "release tag is unavailable in the checkout: $TAG"
    [ "$TAG_COMMIT" = "$SOURCE_COMMIT" ] || \
        die "release tag $TAG does not point at checked-out commit $SOURCE_COMMIT"
fi

install -d -m 0755 "$(dirname "$FIXED_WORKTREE")"
exec 9>"${FIXED_WORKTREE}.lock"
flock -n 9 || die "agent-memory source worktree is already in use"

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
git -C "$SOURCE_REPO" worktree lock --reason 'agent-memory prebuilt package build' \
    "$FIXED_WORKTREE"

COMPONENT_ROOT="$FIXED_WORKTREE/src/agent-memory"
COMPONENT_VERSION="$(
    python3 "$COMPONENT_ROOT/packaging/raw/verify-release.py" \
        "$COMPONENT_ROOT"
)"
[ "$VERSION" = "$COMPONENT_VERSION" ] || \
    die "requested version $VERSION does not match agent-memory $COMPONENT_VERSION"

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

[ "$(node --version)" = v22.23.2 ] || die 'Node 22.23.2 is required'
PLUGIN_ROOT="$COMPONENT_ROOT/adapters/agent-memory/openclaw"
make -C "$COMPONENT_ROOT" build-openclaw-plugin-ci
(cd "$PLUGIN_ROOT" && npm test)
git -C "$FIXED_WORKTREE" diff --exit-code

AGENT_MEMORY_HOST_CARGO="$(command -v cargo)"
export AGENT_MEMORY_HOST_CARGO
export PATH="$ACTION_DIR/host-tools:$PATH"
(
    cd "$FIXED_WORKTREE"
    python3 "$COMMON_DIR/reproducible-build.py" \
        --source-root "$COMPONENT_ROOT" --source-date-epoch "$SOURCE_DATE_EPOCH" \
        -- "$COMMON_DIR/cross-profile.sh" "$PROFILE" build --release --locked \
        --bin agent-memory --manifest-path src/agent-memory/Cargo.toml
)
BIN_DIR="$COMPONENT_ROOT/target/$RUST_TARGET/release"
python3 "$COMMON_DIR/verify-glibc.py" --arch "$TARGET_ARCH" --max 2.28 "$BIN_DIR/agent-memory"
METADATA="$BIN_DIR/agent-memory-build.toml"
{
    printf 'version = "%s"\n' "$VERSION"
    printf 'target_os = "linux"\ntarget_arch = "%s"\n[binaries]\n' "$TARGET_ARCH"
    printf 'agent-memory = "%s"\n' "$(sha256sum "$BIN_DIR/agent-memory" | awk '{print $1}')"
    printf '[plugin]\nsha256 = "%s"\n' "$(sha256sum "$PLUGIN_ROOT/dist/index.js" | awk '{print $1}')"
} > "$METADATA"
printf 'Source commit: %s\n' "$SOURCE_COMMIT"
cat "$METADATA"
ARCHIVE="agent-memory-$VERSION-linux-$TARGET_ARCH.tar.gz"
for attempt in 1 2; do
    python3 "$COMPONENT_ROOT/packaging/raw/package.py" \
        --bin-dir "$BIN_DIR" --plugin-dir "$PLUGIN_ROOT/dist" --metadata "$METADATA" \
        --target-arch "$TARGET_ARCH" --output-dir "$OUTPUT_DIR" --epoch "$SOURCE_DATE_EPOCH"
    digest="$(sha256sum "$OUTPUT_DIR/$ARCHIVE" | awk '{print $1}')"
    if [ "$attempt" = 1 ]; then first_digest="$digest"
    else [ "$digest" = "$first_digest" ] || die 'repeated packaging changed the archive'; fi
done
(cd "$OUTPUT_DIR" && sha256sum "$ARCHIVE" > "$ARCHIVE.sha256")
python3 "$ACTION_DIR/plugin-sbom.py" "$PLUGIN_ROOT" "$BIN_DIR/plugin-sbom.json"
python3 "$COMMON_DIR/generate-sbom.py" \
    --artifact "$OUTPUT_DIR/$ARCHIVE" --component agent-memory --version "$VERSION" \
    --os linux --arch "$TARGET_ARCH" --target "$RUST_TARGET" --project-dir "$COMPONENT_ROOT" \
    --extra-sbom "$BIN_DIR/plugin-sbom.json" --source-date-epoch "$SOURCE_DATE_EPOCH" >/dev/null
python3 "$COMMON_DIR/verify-artifacts.py" --directory "$OUTPUT_DIR" \
    --component agent-memory --version "$VERSION" --layout flat --os linux --arch "$TARGET_ARCH"
printf 'Built agent-memory %s for linux/%s at %s\n' "$VERSION" "$TARGET_ARCH" "$OUTPUT_DIR"
