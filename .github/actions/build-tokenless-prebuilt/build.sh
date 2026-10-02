#!/usr/bin/env bash
# Build and package one reproducible Tokenless precompiled release target.
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMON_DIR="$(cd "$ACTION_DIR/../prebuilt-rust-common" && pwd)"
FIXED_WORKTREE="${TOKENLESS_SOURCE_WORKTREE:-/tmp/anolisa-raw-release-source-worktrees/tokenless}"
VERSION=""
TARGET_OS=""
TARGET_ARCH=""
PROFILE=""
TAG=""
SOURCE_REPO=""
OUTPUT_DIR=""
WHEEL_OUTPUT_DIR=""
WORKTREE_READY=0
MATURIN_VERSION=1.15.0
PYPROJECT_BUILD_VERSION=1.3.0

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: build.sh --source-repo PATH --output-dir PATH --wheel-output-dir PATH \
  --version X.Y.Z \
  --target-os {linux|macos} --target-arch {x86_64|aarch64} \
  --profile PROFILE [--tag tokenless/vX.Y.Z]
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --source-repo) SOURCE_REPO="${2:-}"; shift 2 ;;
        --output-dir) OUTPUT_DIR="${2:-}"; shift 2 ;;
        --wheel-output-dir) WHEEL_OUTPUT_DIR="${2:-}"; shift 2 ;;
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
[ -n "$WHEEL_OUTPUT_DIR" ] || die '--wheel-output-dir is required'
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || \
    die "invalid Tokenless version: $VERSION"

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

if [ -n "$TAG" ] && [ "$TAG" != "tokenless/v$VERSION" ]; then
    die "release tag $TAG does not match requested version $VERSION"
fi
for command in cargo flock git install just make node npm patch python3 sha256sum uvx; do
    command -v "$command" >/dev/null || die "missing required command: $command"
done
SOURCE_REPO="$(git -C "$SOURCE_REPO" rev-parse --show-toplevel)" || \
    die "source-repo is not a Git worktree"
SOURCE_COMMIT="$(git -C "$SOURCE_REPO" rev-parse HEAD)"

if [ -n "$TAG" ]; then
    SOURCE_COMMIT="$(git -C "$SOURCE_REPO" rev-parse --verify "refs/tags/$TAG^{commit}")" || \
        die "release tag is unavailable in the checkout: $TAG"
fi

install -d -m 0755 "$(dirname "$FIXED_WORKTREE")"
exec 9>"${FIXED_WORKTREE}.lock"
flock -n 9 || die "Tokenless source worktree is already in use"

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
git -C "$SOURCE_REPO" worktree lock --reason 'Tokenless prebuilt package build' \
    "$FIXED_WORKTREE"

COMPONENT_ROOT="$FIXED_WORKTREE/src/tokenless"
(
    cd "$COMPONENT_ROOT"
    make -B stamp-adapter-templates stamp-python-packages
)
CONTRACT="$COMPONENT_ROOT/.anolisa/component.toml"
SOURCE_VERSION="$(
    python3 "$COMPONENT_ROOT/packaging/raw/verify-release.py" \
        "$COMPONENT_ROOT" "$CONTRACT"
)"
[ "$VERSION" = "$SOURCE_VERSION" ] || \
    die "requested version $VERSION does not match Tokenless $SOURCE_VERSION"
(
    cd "$COMPONENT_ROOT"
    make build-openclaw-plugin build-dsh-plugin
    bash "$ACTION_DIR/setup-rtk.sh" "$COMPONENT_ROOT"
)
RTK_ROOT="$COMPONENT_ROOT/third_party/rtk"
# RTK is excluded from Tokenless's workspace; make its cloned source an explicit
# workspace root so Cargo never searches the persistent /tmp parent hierarchy.
printf '\n[workspace]\n' >> "$RTK_ROOT/Cargo.toml"

if [ -e "$OUTPUT_DIR" ] && [ -n "$(find "$OUTPUT_DIR" -mindepth 1 -print -quit)" ]; then
    die "output directory must be empty: $OUTPUT_DIR"
fi
if [ -e "$WHEEL_OUTPUT_DIR" ] && \
    [ -n "$(find "$WHEEL_OUTPUT_DIR" -mindepth 1 -print -quit)" ]; then
    die "wheel output directory must be empty: $WHEEL_OUTPUT_DIR"
fi
install -d -m 0755 "$OUTPUT_DIR"
install -d -m 0755 "$WHEEL_OUTPUT_DIR"
SOURCE_DATE_EPOCH="$(git -C "$FIXED_WORKTREE" show -s --format=%ct HEAD)"
case "$SOURCE_DATE_EPOCH" in
    '' | *[!0-9]*) die 'source commit timestamp is not numeric' ;;
esac
export RUSTUP_TOOLCHAIN=1.93.0-x86_64-unknown-linux-gnu
export SOURCE_DATE_EPOCH

for manifest in "$COMPONENT_ROOT/Cargo.toml" "$RTK_ROOT/Cargo.toml"; do
    cargo metadata \
        --format-version 1 \
        --locked \
        --filter-platform "$RUST_TARGET" \
        --manifest-path "$manifest" >/dev/null
done

(
    cd "$FIXED_WORKTREE"
    python3 "$COMMON_DIR/reproducible-build.py" \
        --source-root "$COMPONENT_ROOT" \
        --source-date-epoch "$SOURCE_DATE_EPOCH" \
        -- "$COMMON_DIR/cross-profile.sh" "$PROFILE" build \
        --release \
        --locked \
        --manifest-path src/tokenless/Cargo.toml
    python3 "$COMMON_DIR/reproducible-build.py" \
        --source-root "$COMPONENT_ROOT" \
        --source-date-epoch "$SOURCE_DATE_EPOCH" \
        -- "$COMMON_DIR/cross-profile.sh" "$PROFILE" build \
        --release \
        --locked \
        --manifest-path src/tokenless/third_party/rtk/Cargo.toml
)

HOST_CARGO="$(command -v cargo)"
CARGO_SHIM_DIR="$COMPONENT_ROOT/target/maturin-cargo-shim"
install -d -m 0755 "$CARGO_SHIM_DIR"
ln -s "$ACTION_DIR/cargo-shim.sh" "$CARGO_SHIM_DIR/cargo"
PYTHON_RTK_DIR="$COMPONENT_ROOT/python/tokenless/python/anolisa_tokenless/_bin"
(
    set -e
    install -d -m 0755 "$PYTHON_RTK_DIR"
    install -p -m 0755 \
        "$RTK_ROOT/target/$RUST_TARGET/release/rtk" \
        "$PYTHON_RTK_DIR/rtk"
    trap 'rm -f "$PYTHON_RTK_DIR/rtk"' EXIT
    cd "$COMPONENT_ROOT/python/tokenless"
    compatibility=(--compatibility pypi)
    if [ "$TARGET_OS" = linux ]; then
        compatibility=(--compatibility manylinux2014)
    else
        export MACOSX_DEPLOYMENT_TARGET=11.0
    fi
    PATH="$CARGO_SHIM_DIR:$PATH" \
    TOKENLESS_HOST_CARGO="$HOST_CARGO" \
    TOKENLESS_CROSS_PROFILE_SCRIPT="$COMMON_DIR/cross-profile.sh" \
    TOKENLESS_CROSS_PROFILE="$PROFILE" \
    TOKENLESS_RUST_TARGET="$RUST_TARGET" \
    TOKENLESS_CARGO_MANIFEST="$COMPONENT_ROOT/python/tokenless/Cargo.toml" \
    TOKENLESS_CROSS_PROJECT_ROOT="$COMPONENT_ROOT" \
    TOKENLESS_CARGO_OUTPUT_REWRITER="$ACTION_DIR/rewrite-cross-cargo-output.py" \
        python3 "$COMMON_DIR/reproducible-build.py" \
            --source-root "$COMPONENT_ROOT" \
            --source-date-epoch "$SOURCE_DATE_EPOCH" \
            -- uvx --from "maturin==$MATURIN_VERSION" maturin build \
            --profile python-release \
            --interpreter python3 \
            --target "$RUST_TARGET" \
            --locked \
            "${compatibility[@]}" \
            --auditwheel check \
            --out "$WHEEL_OUTPUT_DIR"
)
if [ "$TARGET_OS/$TARGET_ARCH" = linux/x86_64 ]; then
    uvx --from "build==$PYPROJECT_BUILD_VERSION" pyproject-build \
        --wheel \
        --outdir "$WHEEL_OUTPUT_DIR" \
        "$COMPONENT_ROOT/python/agentscope"
fi
python3 "$ACTION_DIR/verify-wheels.py" \
    --directory "$WHEEL_OUTPUT_DIR" \
    --version "$VERSION" \
    --layout flat \
    --os "$TARGET_OS" \
    --arch "$TARGET_ARCH"

BIN_DIR="$COMPONENT_ROOT/target/raw-prebuilt/$RUST_TARGET"
install -d -m 0755 "$BIN_DIR"
install -p -m 0755 \
    "$COMPONENT_ROOT/target/$RUST_TARGET/release/tokenless" \
    "$BIN_DIR/tokenless"
install -p -m 0755 \
    "$RTK_ROOT/target/$RUST_TARGET/release/rtk" \
    "$BIN_DIR/rtk"
for binary in tokenless rtk; do
    if [ "$TARGET_OS" = macos ]; then
        python3 "$COMMON_DIR/verify-macho.py" --min 11.0 "$BIN_DIR/$binary"
    else
        python3 "$COMMON_DIR/verify-glibc.py" \
            --arch "$TARGET_ARCH" --max 2.17 "$BIN_DIR/$binary"
    fi
done

TOKENLESS_SOURCE_DIR="$COMPONENT_ROOT" \
RAW_CONTRACT="$CONTRACT" \
BIN_DIR="$BIN_DIR" \
OUTPUT_DIR="$OUTPUT_DIR" \
TARGET_OS="$TARGET_OS" \
TARGET_ARCH="$TARGET_ARCH" \
SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
    "$COMPONENT_ROOT/packaging/raw/package.sh" package >/dev/null

ARCHIVE="tokenless-$VERSION-$TARGET_OS-$TARGET_ARCH.tar.gz"
(
    cd "$OUTPUT_DIR"
    sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
)
python3 "$COMMON_DIR/generate-sbom.py" \
    --artifact "$OUTPUT_DIR/$ARCHIVE" \
    --component tokenless \
    --version "$VERSION" \
    --os "$TARGET_OS" \
    --arch "$TARGET_ARCH" \
    --target "$RUST_TARGET" \
    --project-dir "$COMPONENT_ROOT" \
    --project-dir "$RTK_ROOT" \
    --source-date-epoch "$SOURCE_DATE_EPOCH" >/dev/null
python3 "$COMMON_DIR/verify-artifacts.py" \
    --directory "$OUTPUT_DIR" \
    --component tokenless \
    --version "$VERSION" \
    --layout flat \
    --os "$TARGET_OS" \
    --arch "$TARGET_ARCH"
printf 'Built Tokenless %s for %s/%s at %s\n' \
    "$VERSION" "$TARGET_OS" "$TARGET_ARCH" "$OUTPUT_DIR"
printf 'Built Tokenless Python wheels for %s/%s at %s\n' \
    "$TARGET_OS" "$TARGET_ARCH" "$WHEEL_OUTPUT_DIR"
