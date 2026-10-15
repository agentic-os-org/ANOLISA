#!/usr/bin/env bash
# Assemble skillfs runtime files without building or executing cross binaries.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ROOT="${SKILLFS_SOURCE_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
BIN_DIR="${BIN_DIR:-$SOURCE_ROOT/target/release}"
CONTRACT="${RAW_CONTRACT:-$SOURCE_ROOT/.anolisa/component.toml}"
TARGET_OS="${TARGET_OS:-linux}"
TARGET_ARCH="${TARGET_ARCH:-$(uname -m)}"
BUILD_METADATA="${SKILLFS_BUILD_METADATA:-$BIN_DIR/skillfs-build.toml}"
COMMAND="${1:-}"

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

case "$COMMAND" in stage | package) ;; *) die "usage: $0 {stage|package}" ;; esac
case "$TARGET_OS/$TARGET_ARCH" in
    linux/x86_64 | linux/aarch64) ;;
    *) die "unsupported skillfs target: $TARGET_OS/$TARGET_ARCH" ;;
esac
[ -x "$BIN_DIR/skillfs" ] || die "missing executable: $BIN_DIR/skillfs"
VERSION="$(python3 "$SCRIPT_DIR/verify-release.py" "$SOURCE_ROOT" "$CONTRACT" \
    --os "$TARGET_OS" --arch "$TARGET_ARCH")"

if [ -n "${SKILLFS_BUILD_METADATA:-}" ] || [ -f "$BUILD_METADATA" ]; then
    python3 "$SCRIPT_DIR/verify-binaries.py" "$BIN_DIR/skillfs" \
        --arch "$TARGET_ARCH" --metadata "$BUILD_METADATA" --component-version "$VERSION"
else
    [ "$(uname -s)/$(uname -m)" = "Linux/$TARGET_ARCH" ] \
        || die "cross-target packaging requires build metadata: $BUILD_METADATA"
    python3 "$SCRIPT_DIR/verify-binaries.py" "$BIN_DIR/skillfs" \
        --arch "$TARGET_ARCH" --component-version "$VERSION"
    REPORTED="$("$BIN_DIR/skillfs" --version)"
    [ "$REPORTED" = "skillfs $VERSION" ] || die "binary reports $REPORTED, expected $VERSION"
fi

stage_payload() {
    local stage="$1"
    if [ -d "$stage" ] && [ -n "$(find "$stage" -mindepth 1 -print -quit)" ]; then
        die "staging directory must be empty: $stage"
    fi
    local skills="$stage/share/anolisa/runtime/skills/skillfs"
    install -d -m 0755 "$stage/.anolisa" "$stage/bin" "$skills" "$stage/share/doc/skillfs"
    install -m 0644 "$CONTRACT" "$stage/.anolisa/component.toml"
    install -m 0755 "$BIN_DIR/skillfs" "$stage/bin/skillfs"
    cp -pr "$SOURCE_ROOT/docs/skills/skillfs-mount/." "$skills/"
    # LICENSE is a source symlink in the monorepo; install copies its bytes.
    install -m 0644 "$SOURCE_ROOT/LICENSE" "$SOURCE_ROOT/README.md" "$stage/share/doc/skillfs/"
    find "$stage" -type d -exec chmod 0755 {} +
    find "$stage" -type f -exec chmod 0644 {} +
    chmod 0755 "$stage/bin/skillfs"
    [ -z "$(find "$stage" -type l -print -quit)" ] || die "raw payload contains a symlink"
}

if [ "$COMMAND" = stage ]; then
    [ -n "${DESTDIR:-}" ] || die "DESTDIR is required by stage"
    stage_payload "$DESTDIR"
    exit 0
fi
OUTPUT_DIR="${OUTPUT_DIR:-$SOURCE_ROOT/target/raw}"
EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$SOURCE_ROOT" log -1 --format=%ct -- .)}"
case "$EPOCH" in '' | *[!0-9]*) die "SOURCE_DATE_EPOCH must be a non-negative integer" ;; esac
tar --version | grep -q 'GNU tar' || die "GNU tar is required for reproducible packages"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
stage_payload "$WORK/stage"
install -d -m 0755 "$OUTPUT_DIR"
ARTIFACT="skillfs-$VERSION-$TARGET_OS-$TARGET_ARCH.tar.gz"
LC_ALL=C tar --sort=name --mtime="@$EPOCH" --owner=0 --group=0 --numeric-owner \
    --hard-dereference --format=gnu -C "$WORK/stage" -cf - . | gzip -n -9 > "$WORK/$ARTIFACT"
mv "$WORK/$ARTIFACT" "$OUTPUT_DIR/$ARTIFACT"
printf '%s\n' "$OUTPUT_DIR/$ARTIFACT"
