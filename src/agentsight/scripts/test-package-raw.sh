#!/usr/bin/env bash
# Verify AgentSight component-owned Linux and macOS raw packages.
set -euo pipefail
unset ENFORCER_BIN

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
    python3 - "$1" "${2:-62}" <<'PY'
import pathlib
import struct
import sys

header = bytearray(64)
header[:4] = b"\x7fELF"
header[4] = 2
header[5] = 1
struct.pack_into("<H", header, 18, int(sys.argv[2]))
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
sha256sum "$BIN_DIR/agentsight-enforcer" | cut -d ' ' -f 1 > "$BIN_DIR/agentsight-enforcer.sha256"
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
    ./.anolisa/component.toml | cmp - "$SOURCE_ROOT/packaging/anolisa/raw/component.toml"
tar -xzOf "$TMP_ROOT/linux-one/$LINUX_ARTIFACT" \
    ./share/anolisa/agentsight/agentsight.service | grep -Fxq 'ExecStart={bindir}/agentsight-start'
tar -xzOf "$TMP_ROOT/linux-one/$LINUX_ARTIFACT" \
    ./share/anolisa/agentsight/agentsight-enforcer.service | grep -Fxq 'ExecStart={bindir}/agentsight-enforcer'
tar -xzOf "$TMP_ROOT/linux-one/$LINUX_ARTIFACT" \
    ./bin/agentsight-start > "$TMP_ROOT/raw-launcher"
grep -Fxq '    "$(dirname -- "$0")/agentsight" trace &' "$TMP_ROOT/raw-launcher"
if grep -Eq '\{bindir\}|@AGENTSIGHT_COMMAND@|/usr/(local/)?bin/agentsight' "$TMP_ROOT/raw-launcher"; then
    printf 'ERROR: raw launcher contains an unresolved or fixed worker path\n' >&2
    exit 1
fi
python3 - "$SOURCE_ROOT/packaging/anolisa/raw/component.toml" <<'PY'
import pathlib
import sys
import tomllib

contract = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
files = {item["source"]: item for item in contract["component"]["layout"]["files"]}
for unit in ("agentsight.service", "agentsight-enforcer.service"):
    assert files[f"share/anolisa/agentsight/{unit}"]["render"] == "anolisa-paths-v1"
assert "render" not in files["bin/agentsight-start"]
minimum = contract["component"]["contract"]["min_anolisa_version"]
assert tuple(map(int, minimum.split("."))) >= (0, 2, 17)
PY

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

# Neither missing nor stale receipts may be replaced by an implicit native build.
mv "$BIN_DIR/agentsight-enforcer.sha256" "$TMP_ROOT/receipt"
for receipt_state in missing stale; do
    if [ "$receipt_state" = stale ]; then
        printf '%064d\n' 0 > "$BIN_DIR/agentsight-enforcer.sha256"
    fi
    if AGENTSIGHT_SOURCE_DIR="$SOURCE_ROOT" \
        AGENTSIGHT_BUILD_METADATA="$BUILD_METADATA" BIN_DIR="$BIN_DIR" \
        DESTDIR="$TMP_ROOT/$receipt_state-receipt" TARGET_OS=linux TARGET_ARCH=x86_64 \
        "$SOURCE_ROOT/packaging/raw/package.sh" stage >"$TMP_ROOT/receipt.out" 2>&1; then
        printf 'ERROR: raw package accepted a %s receipt\n' "$receipt_state" >&2
        exit 1
    fi
done
mv "$TMP_ROOT/receipt" "$BIN_DIR/agentsight-enforcer.sha256"

# Explicit private artifacts override BIN_DIR, including cross-build metadata.
# Replacing the old shared location must not affect either independent payload.
printf 'unattested shared replacement\n' > "$BIN_DIR/agentsight-enforcer"
for invocation in one two; do
    private_dir="$(mktemp -d "$TMP_ROOT/private.XXXXXXXX")"
    private_bin="$private_dir/agentsight-enforcer"
    write_elf "$private_bin"
    printf '%s\n' "$invocation" >> "$private_bin"
    sha256sum "$private_bin" | cut -d ' ' -f 1 > "$private_bin.sha256"
    write_metadata linux x86_64 "$BIN_DIR/agentsight" "$private_bin"
    ENFORCER_BIN="$private_bin" AGENTSIGHT_SOURCE_DIR="$SOURCE_ROOT" \
        AGENTSIGHT_BUILD_METADATA="$BUILD_METADATA" BIN_DIR="$BIN_DIR" \
        DESTDIR="$TMP_ROOT/private-$invocation" TARGET_OS=linux TARGET_ARCH=x86_64 \
        "$SOURCE_ROOT/packaging/raw/package.sh" stage >/dev/null
    cmp "$private_bin" "$TMP_ROOT/private-$invocation/bin/agentsight-enforcer"
done
! cmp -s "$TMP_ROOT/private-one/bin/agentsight-enforcer" \
    "$TMP_ROOT/private-two/bin/agentsight-enforcer"

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
    ./.anolisa/component.toml | cmp - "$SOURCE_ROOT/packaging/anolisa/raw/component.macos.toml"

if AGENTSIGHT_SOURCE_DIR="$SOURCE_ROOT" \
    AGENTSIGHT_BUILD_METADATA="$BUILD_METADATA" \
    BIN_DIR="$BIN_DIR" \
    OUTPUT_DIR="$TMP_ROOT/unsupported" \
    TARGET_OS=macos \
    TARGET_ARCH=x86_64 \
    SOURCE_DATE_EPOCH=1700000000 \
        "$SOURCE_ROOT/packaging/raw/package.sh" package >/dev/null 2>&1; then
    printf 'ERROR: package accepted unsupported macOS x86_64 target\n' >&2
    exit 1
fi


write_elf "$BIN_DIR/agentsight" 183
write_elf "$BIN_DIR/agentsight-enforcer" 183
sha256sum "$BIN_DIR/agentsight-enforcer" | cut -d ' ' -f 1 > "$BIN_DIR/agentsight-enforcer.sha256"
write_metadata linux aarch64 "$BIN_DIR/agentsight" "$BIN_DIR/agentsight-enforcer"
for output in "$TMP_ROOT/arm-one" "$TMP_ROOT/arm-two"; do
    AGENTSIGHT_BUILD_METADATA="$BUILD_METADATA" BIN_DIR="$BIN_DIR" \
        OUTPUT_DIR="$output" TARGET_OS=linux TARGET_ARCH=aarch64 \
        SOURCE_DATE_EPOCH=1700000000 "$SOURCE_ROOT/packaging/raw/package.sh" package >/dev/null
done
ARM_ARTIFACT="agentsight-$VERSION-linux-aarch64.tar.gz"
cmp "$TMP_ROOT/arm-one/$ARM_ARTIFACT" "$TMP_ROOT/arm-two/$ARM_ARTIFACT"
tar -tzf "$TMP_ROOT/arm-one/$ARM_ARTIFACT" | cmp - "$TMP_ROOT/linux-members"
tar -xzOf "$TMP_ROOT/arm-one/$ARM_ARTIFACT" ./.anolisa/component.toml | \
    cmp - "$SOURCE_ROOT/packaging/anolisa/raw/component.toml"
# A valid receipt does not make a host-architecture enforcer an ARM64 binary.
write_elf "$BIN_DIR/agentsight-enforcer"
sha256sum "$BIN_DIR/agentsight-enforcer" | cut -d ' ' -f 1 > "$BIN_DIR/agentsight-enforcer.sha256"
write_metadata linux aarch64 "$BIN_DIR/agentsight" "$BIN_DIR/agentsight-enforcer"
if AGENTSIGHT_BUILD_METADATA="$BUILD_METADATA" BIN_DIR="$BIN_DIR" \
    OUTPUT_DIR="$TMP_ROOT/wrong-arch" TARGET_OS=linux TARGET_ARCH=aarch64 \
    "$SOURCE_ROOT/packaging/raw/package.sh" package >"$TMP_ROOT/arch-error" 2>&1; then
    echo 'ERROR: ARM64 package accepted an x86_64 enforcer' >&2
    exit 1
fi
grep -Fq 'ELF machine 62 does not match aarch64' "$TMP_ROOT/arch-error"

printf 'AgentSight raw package tests passed\n'
