#!/usr/bin/env bash
# Verify AgentSight component-owned Linux and macOS raw packages.
set -euo pipefail

SOURCE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

VERSION="$(
    python3 - "$SOURCE_ROOT/Cargo.toml" <<'PY'
import pathlib
import sys
import tomllib

with pathlib.Path(sys.argv[1]).open("rb") as stream:
    print(tomllib.load(stream)["package"]["version"])
PY
)"
LINUX_ARTIFACT="agentsight-$VERSION-linux-x86_64.tar.gz"
MACOS_ARTIFACT="agentsight-$VERSION-macos-aarch64.tar.gz"
BIN_DIR="$TMP_ROOT/bin"
BUILD_METADATA="$BIN_DIR/agentsight-build.toml"
mkdir -p "$BIN_DIR"

write_elf() {
    python3 - "$1" <<'PY'
import pathlib
import struct
import sys

header = bytearray(64)
header[:4] = b"\x7fELF"
header[4] = 2
header[5] = 1
struct.pack_into("<H", header, 18, 62)
pathlib.Path(sys.argv[1]).write_bytes(header)
PY
    chmod 0755 "$1"
}

write_macho() {
    python3 - "$1" <<'PY'
import pathlib
import struct
import sys

pathlib.Path(sys.argv[1]).write_bytes(
    struct.pack("<IiiIIIII", 0xFEEDFACF, 0x0100000C, 0, 2, 0, 0, 0, 0)
)
PY
    chmod 0755 "$1"
}

write_metadata() {
    local os_name="$1"
    local arch="$2"
    shift 2
    {
        printf 'version = "%s"\n' "$VERSION"
        printf 'target_os = "%s"\n' "$os_name"
        printf 'target_arch = "%s"\n\n' "$arch"
        printf '[binaries]\n'
        for binary in "$@"; do
            printf '%s = "%s"\n' \
                "$(basename "$binary")" \
                "$(sha256sum "$binary" | awk '{print $1}')"
        done
    } > "$BUILD_METADATA"
}

write_elf "$BIN_DIR/agentsight"
write_elf "$BIN_DIR/agentsight-enforcer"
write_metadata linux x86_64 \
    "$BIN_DIR/agentsight" "$BIN_DIR/agentsight-enforcer"

for output in "$TMP_ROOT/linux-one" "$TMP_ROOT/linux-two"; do
    AGENTSIGHT_SOURCE_DIR="$SOURCE_ROOT" \
    AGENTSIGHT_BUILD_METADATA="$BUILD_METADATA" \
    BIN_DIR="$BIN_DIR" \
    OUTPUT_DIR="$output" \
    TARGET_OS=linux \
    TARGET_ARCH=x86_64 \
    SOURCE_DATE_EPOCH=1700000000 \
        "$SOURCE_ROOT/packaging/raw/package.sh" package >/dev/null
done

cmp \
    "$TMP_ROOT/linux-one/$LINUX_ARTIFACT" \
    "$TMP_ROOT/linux-two/$LINUX_ARTIFACT"
tar -tzf "$TMP_ROOT/linux-one/$LINUX_ARTIFACT" > "$TMP_ROOT/linux-members"
for member in \
    './.anolisa/component.toml' \
    './bin/agentsight' \
    './bin/agentsight-enforcer' \
    './bin/agentsight-start' \
    './share/anolisa/agentsight/agentsight.service' \
    './share/anolisa/agentsight/agentsight-enforcer.service'; do
    grep -Fxq "$member" "$TMP_ROOT/linux-members"
done
tar -xzOf "$TMP_ROOT/linux-one/$LINUX_ARTIFACT" \
    ./.anolisa/component.toml | cmp - "$SOURCE_ROOT/.anolisa/component.toml"

printf 'stale\n' >> "$BIN_DIR/agentsight"
if AGENTSIGHT_SOURCE_DIR="$SOURCE_ROOT" \
    AGENTSIGHT_BUILD_METADATA="$BUILD_METADATA" \
    BIN_DIR="$BIN_DIR" \
    OUTPUT_DIR="$TMP_ROOT/stale-binary" \
    TARGET_OS=linux \
    TARGET_ARCH=x86_64 \
    SOURCE_DATE_EPOCH=1700000000 \
        "$SOURCE_ROOT/packaging/raw/package.sh" package >/dev/null 2>&1; then
    printf 'ERROR: package accepted a binary outside build metadata\n' >&2
    exit 1
fi
write_elf "$BIN_DIR/agentsight"
write_metadata linux x86_64 \
    "$BIN_DIR/agentsight" "$BIN_DIR/agentsight-enforcer"

mv "$BIN_DIR/agentsight-enforcer" "$BIN_DIR/agentsight-enforcer.missing"
if AGENTSIGHT_SOURCE_DIR="$SOURCE_ROOT" \
    AGENTSIGHT_BUILD_METADATA="$BUILD_METADATA" \
    BIN_DIR="$BIN_DIR" \
    OUTPUT_DIR="$TMP_ROOT/missing-enforcer" \
    TARGET_OS=linux \
    TARGET_ARCH=x86_64 \
    SOURCE_DATE_EPOCH=1700000000 \
        "$SOURCE_ROOT/packaging/raw/package.sh" package >/dev/null 2>&1; then
    printf 'ERROR: Linux package accepted a missing agentsight-enforcer\n' >&2
    exit 1
fi
mv "$BIN_DIR/agentsight-enforcer.missing" "$BIN_DIR/agentsight-enforcer"

write_macho "$BIN_DIR/agentsight"
write_metadata macos aarch64 "$BIN_DIR/agentsight"
AGENTSIGHT_SOURCE_DIR="$SOURCE_ROOT" \
AGENTSIGHT_BUILD_METADATA="$BUILD_METADATA" \
BIN_DIR="$BIN_DIR" \
OUTPUT_DIR="$TMP_ROOT/macos" \
TARGET_OS=macos \
TARGET_ARCH=aarch64 \
SOURCE_DATE_EPOCH=1700000000 \
    "$SOURCE_ROOT/packaging/raw/package.sh" package >/dev/null

tar -tzf "$TMP_ROOT/macos/$MACOS_ARTIFACT" > "$TMP_ROOT/macos-members"
grep -Fxq './.anolisa/component.toml' "$TMP_ROOT/macos-members"
grep -Fxq './bin/agentsight' "$TMP_ROOT/macos-members"
if grep -Eq 'agentsight-(start|enforcer)|agentsight(-enforcer)?\.service' \
    "$TMP_ROOT/macos-members"; then
    printf 'ERROR: macOS package contains Linux-only AgentSight files\n' >&2
    exit 1
fi
tar -xzOf "$TMP_ROOT/macos/$MACOS_ARTIFACT" \
    ./.anolisa/component.toml | cmp - "$SOURCE_ROOT/.anolisa/component.macos.toml"

if AGENTSIGHT_SOURCE_DIR="$SOURCE_ROOT" \
    AGENTSIGHT_BUILD_METADATA="$BUILD_METADATA" \
    BIN_DIR="$BIN_DIR" \
    OUTPUT_DIR="$TMP_ROOT/unsupported" \
    TARGET_OS=linux \
    TARGET_ARCH=aarch64 \
    SOURCE_DATE_EPOCH=1700000000 \
        "$SOURCE_ROOT/packaging/raw/package.sh" package >/dev/null 2>&1; then
    printf 'ERROR: package accepted unsupported Linux aarch64 target\n' >&2
    exit 1
fi

printf 'AgentSight raw package tests passed\n'
