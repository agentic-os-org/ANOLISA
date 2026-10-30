#!/usr/bin/env bash
# Build and package one reproducible agentsight precompiled release target.
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMON_DIR="$(cd "$ACTION_DIR/../prebuilt-rust-common" && pwd)"
FIXED_WORKTREE="${AGENTSIGHT_SOURCE_WORKTREE:-/tmp/anolisa-raw-release-source-worktrees/agentsight}"
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
  --profile PROFILE [--tag sight/vX.Y.Z]
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
    macos/aarch64/darwin11-aarch64)
        RUST_TARGET=aarch64-apple-darwin
        ;;
    *)
        die "profile $PROFILE does not match target $TARGET_OS/$TARGET_ARCH"
        ;;
esac

if [ -n "$TAG" ]; then
    [ "$TAG" = "sight/v$VERSION" ] || \
        die "release tag $TAG does not match requested version $VERSION"
    TAG_COMMIT="$(git -C "$SOURCE_REPO" rev-parse --verify "refs/tags/$TAG^{commit}")" || \
        die "release tag is unavailable in the checkout: $TAG"
    [ "$TAG_COMMIT" = "$SOURCE_COMMIT" ] || \
        die "release tag $TAG does not point at checked-out commit $SOURCE_COMMIT"
fi

install -d -m 0755 "$(dirname "$FIXED_WORKTREE")"
exec 9>"${FIXED_WORKTREE}.lock"
flock -n 9 || die "agentsight source worktree is already in use"

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
git -C "$SOURCE_REPO" worktree lock --reason 'agentsight prebuilt package build' \
    "$FIXED_WORKTREE"

COMPONENT_ROOT="$FIXED_WORKTREE/src/agentsight"
CONTRACT_DIR="$COMPONENT_ROOT/packaging/anolisa/raw"
# Historical tagged sources retain their original contract directory.
[ -d "$CONTRACT_DIR" ] || CONTRACT_DIR="$COMPONENT_ROOT/.anolisa"
LINUX_VERSION="$(
    python3 "$COMPONENT_ROOT/packaging/raw/verify-release.py" \
        "$COMPONENT_ROOT" "$CONTRACT_DIR/component.toml" \
        --os linux --arch x86_64
)"
MACOS_VERSION="$(
    python3 "$COMPONENT_ROOT/packaging/raw/verify-release.py" \
        "$COMPONENT_ROOT" "$CONTRACT_DIR/component.macos.toml" \
        --os macos --arch aarch64
)"
[ "$LINUX_VERSION" = "$MACOS_VERSION" ] || \
    die "Linux and macOS contracts do not have one version"
[ "$VERSION" = "$LINUX_VERSION" ] || \
    die "requested version $VERSION does not match agentsight $LINUX_VERSION"

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
AGENTSIGHT_HOST_CARGO="$(command -v cargo)"
export AGENTSIGHT_HOST_CARGO
export PATH="$ACTION_DIR/host-tools:$PATH"

# Build the embedded viewer from the lockfile in the same source revision.
(cd "$COMPONENT_ROOT/dashboard" && npm ci && npm run build:embed)
BUILD_ARGS=(--release --locked --bin agentsight --manifest-path src/agentsight/Cargo.toml)
SBOM_ARGS=(--project-dir "$COMPONENT_ROOT")
if [ "$TARGET_OS" = macos ]; then
    BUILD_ARGS+=(--no-default-features --features server)
    SBOM_ARGS+=(--no-default-features --features server)
fi
(
    cd "$FIXED_WORKTREE"
    python3 "$COMMON_DIR/reproducible-build.py" \
        --source-root "$COMPONENT_ROOT" --source-date-epoch "$SOURCE_DATE_EPOCH" \
        -- "$COMMON_DIR/cross-profile.sh" "$PROFILE" build "${BUILD_ARGS[@]}"
)
BIN_DIR="$COMPONENT_ROOT/target/$RUST_TARGET/release"
BINARIES=("$BIN_DIR/agentsight")
ENFORCER_BIN=""
if [ "$TARGET_OS" = linux ]; then
    # Keep enforcer dependency resolution separate from the main binary's lockfile.
    mkdir "$FIXED_WORKTREE/.enforcer-source"
    git -C "$FIXED_WORKTREE" archive "$SOURCE_COMMIT" src/agentsight | \
        tar -x -C "$FIXED_WORKTREE/.enforcer-source"
    ENFORCER_ROOT="$FIXED_WORKTREE/.enforcer-source/src/agentsight"
    export ACTPLANE_SOURCE_DIR="$ENFORCER_ROOT/target/actplane-src/pinned"
    mkdir -p "$ENFORCER_ROOT/.cargo"
    printf '[patch."https://github.com/eunomia-bpf/ActPlane.git"]\nebpf-ifc-engine.path = "%s/bpf"\n' \
        "$ACTPLANE_SOURCE_DIR" > "$ENFORCER_ROOT/.cargo/config.toml"
    SBOM_ARGS+=(--project-dir "$ENFORCER_ROOT/crates/agentsight-enforcer" \
        --features agentsight-enforcer/actplane)
    # Cross must see the attested patched source at the exact host path.
    mkdir -p "$(dirname "$ACTPLANE_SOURCE_DIR")"
    export CROSS_CONTAINER_OPTS="${CROSS_CONTAINER_OPTS:-} --volume=$ACTPLANE_SOURCE_DIR:$ACTPLANE_SOURCE_DIR:ro"
    export AGENTSIGHT_CROSS_PROFILE="$PROFILE"
    export AGENTSIGHT_COMMON_DIR="$COMMON_DIR"
    ENFORCER_BIN="$(CARGO="$ACTION_DIR/enforcer-cargo.sh" \
        "$ENFORCER_ROOT/scripts/build-enforcer.sh" --target "$RUST_TARGET")"
    # Preserve the builder's receipt; package.sh validates it before copying.
    BINARIES+=("$ENFORCER_BIN")
fi
for binary in "${BINARIES[@]}"; do
    if [ "$TARGET_OS" = macos ]; then
        python3 "$COMMON_DIR/verify-macho.py" --arch "$TARGET_ARCH" --min 11.0 "$binary"
    else
        python3 "$COMMON_DIR/verify-glibc.py" --arch "$TARGET_ARCH" --max 2.28 "$binary"
    fi
done
METADATA="$BIN_DIR/agentsight-build.toml"
{
    printf 'version = "%s"\n' "$VERSION"
    printf 'target_os = "%s"\n' "$TARGET_OS"
    printf 'target_arch = "%s"\n\n' "$TARGET_ARCH"
    printf '[binaries]\n'
    for binary in "${BINARIES[@]}"; do
        printf '%s = "%s"\n' "$(basename "$binary")" "$(sha256sum "$binary" | awk '{print $1}')"
    done
} > "$METADATA"
printf 'Source commit: %s\n' "$SOURCE_COMMIT"
cat "$METADATA"

if [ "$TARGET_OS" = macos ]; then
    CONTRACT="$CONTRACT_DIR/component.macos.toml"
else
    CONTRACT="$CONTRACT_DIR/component.toml"
fi
ARCHIVE="agentsight-$VERSION-$TARGET_OS-$TARGET_ARCH.tar.gz"
for attempt in 1 2; do
    AGENTSIGHT_SOURCE_DIR="$COMPONENT_ROOT" \
    RAW_CONTRACT="$CONTRACT" \
    BIN_DIR="$BIN_DIR" \
    ENFORCER_BIN="$ENFORCER_BIN" \
    AGENTSIGHT_BUILD_METADATA="$METADATA" \
    OUTPUT_DIR="$OUTPUT_DIR" \
    TARGET_OS="$TARGET_OS" \
    TARGET_ARCH="$TARGET_ARCH" \
    SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
        "$COMPONENT_ROOT/packaging/raw/package.sh" package >/dev/null
    digest="$(sha256sum "$OUTPUT_DIR/$ARCHIVE" | awk '{print $1}')"
    if [ "$attempt" = 1 ]; then
        first_digest="$digest"
    else
        [ "$digest" = "$first_digest" ] || die 'repeated packaging changed the archive'
    fi
done

# Only this invocation's private output is removed, after successful packaging.
if [ -n "$ENFORCER_BIN" ]; then
    rm -rf "$(dirname "$ENFORCER_BIN")"
fi

(
    cd "$OUTPUT_DIR"
    sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
)
python3 "$COMMON_DIR/generate-sbom.py" \
    --artifact "$OUTPUT_DIR/$ARCHIVE" \
    --component agentsight \
    --version "$VERSION" \
    --os "$TARGET_OS" \
    --arch "$TARGET_ARCH" \
    --target "$RUST_TARGET" \
    "${SBOM_ARGS[@]}" \
    --source-date-epoch "$SOURCE_DATE_EPOCH" >/dev/null
python3 "$COMMON_DIR/verify-artifacts.py" \
    --directory "$OUTPUT_DIR" \
    --component agentsight \
    --version "$VERSION" \
    --layout flat \
    --os "$TARGET_OS" \
    --arch "$TARGET_ARCH"
printf 'Built agentsight %s for %s/%s at %s\n' \
    "$VERSION" "$TARGET_OS" "$TARGET_ARCH" "$OUTPUT_DIR"
