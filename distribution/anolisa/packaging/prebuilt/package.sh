#!/usr/bin/env bash
# Assemble one verified ANOLISA CLI binary into a reproducible release archive.
set -euo pipefail

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

[ "${1:-}" = package ] || die "usage: $0 package"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ROOT="${ANOLISA_SOURCE_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
BIN_DIR="${BIN_DIR:-$SOURCE_ROOT/target/release}"
OUTPUT_DIR="${OUTPUT_DIR:-$SOURCE_ROOT/target/prebuilt}"
TARGET_OS="${TARGET_OS:-}"
TARGET_ARCH="${TARGET_ARCH:-}"
BUILD_METADATA="${ANOLISA_BUILD_METADATA:-$BIN_DIR/anolisa-build.toml}"
EPOCH="${SOURCE_DATE_EPOCH:-}"

case "$TARGET_OS/$TARGET_ARCH" in
    linux/x86_64 | linux/aarch64 | macos/aarch64) ;;
    *) die "unsupported ANOLISA CLI target: $TARGET_OS/$TARGET_ARCH" ;;
esac
case "$EPOCH" in
    '' | *[!0-9]*) die "SOURCE_DATE_EPOCH must be a non-negative integer" ;;
esac
[ -x "$BIN_DIR/anolisa" ] || die "missing executable: $BIN_DIR/anolisa"
[ ! -L "$BIN_DIR/anolisa" ] || die "ANOLISA CLI binary must not be a symbolic link"
[ -f "$BUILD_METADATA" ] || die "missing build metadata: $BUILD_METADATA"
tar --version 2>/dev/null | grep -q 'GNU tar' || \
    die "GNU tar is required for reproducible prebuilt packages"

VERSION="$(
    python3 "$SCRIPT_DIR/verify-release.py" "$SOURCE_ROOT" \
        --os "$TARGET_OS" --arch "$TARGET_ARCH"
)"
python3 "$SCRIPT_DIR/verify-binary.py" \
    --metadata "$BUILD_METADATA" \
    --version "$VERSION" \
    --os "$TARGET_OS" \
    --arch "$TARGET_ARCH" \
    "$BIN_DIR/anolisa"

WORK="$(mktemp -d)"
TEMP_ARTIFACT=""
cleanup() {
    rm -rf "$WORK"
    if [ -n "$TEMP_ARTIFACT" ]; then
        rm -f "$TEMP_ARTIFACT"
    fi
}
trap cleanup EXIT

STAGE="$WORK/stage"
install -d -m 0755 "$STAGE" "$OUTPUT_DIR"
install -p -m 0755 "$BIN_DIR/anolisa" "$STAGE/anolisa"
ARTIFACT="anolisa-$VERSION-$TARGET_OS-$TARGET_ARCH.tar.gz"
TEMP_ARTIFACT="$OUTPUT_DIR/.$ARTIFACT.tmp.$$"
LC_ALL=C tar \
    --sort=name \
    --mtime="@$EPOCH" \
    --owner=0 \
    --group=0 \
    --numeric-owner \
    --hard-dereference \
    --format=gnu \
    -C "$STAGE" \
    -cf - . | gzip -n -9 > "$TEMP_ARTIFACT"
mv -f "$TEMP_ARTIFACT" "$OUTPUT_DIR/$ARTIFACT"
TEMP_ARTIFACT=""
printf '%s\n' "$OUTPUT_DIR/$ARTIFACT"
