#!/usr/bin/env bash
# Exercise the component-owned ANOLISA CLI prebuilt packer without compiling.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKER="$ROOT/packaging/prebuilt/package.sh"
VERIFIER="$ROOT/packaging/prebuilt/verify-release.py"
TEMPORARY="$(mktemp -d)"
trap 'rm -rf -- "$TEMPORARY"' EXIT

VERSION="$(python3 "$VERIFIER" "$ROOT" --os linux --arch x86_64)"
BIN_DIR="$TEMPORARY/bin"
install -d -m 0755 "$BIN_DIR"
printf '#!/usr/bin/env sh\nexit 0\n' > "$BIN_DIR/anolisa"
chmod 0755 "$BIN_DIR/anolisa"
DIGEST="$(sha256sum "$BIN_DIR/anolisa" | awk '{print $1}')"

write_metadata() {
    local os_name="$1"
    local arch="$2"
    local path="$3"

    cat > "$path" <<EOF
version = "$VERSION"
target_os = "$os_name"
target_arch = "$arch"

[binaries]
anolisa = "$DIGEST"
EOF
}

run_pack() {
    local os_name="$1"
    local arch="$2"
    local output="$3"
    local metadata="$TEMPORARY/$os_name-$arch.toml"

    write_metadata "$os_name" "$arch" "$metadata"
    ANOLISA_SOURCE_DIR="$ROOT" \
    ANOLISA_BUILD_METADATA="$metadata" \
    BIN_DIR="$BIN_DIR" \
    OUTPUT_DIR="$output" \
    TARGET_OS="$os_name" \
    TARGET_ARCH="$arch" \
    SOURCE_DATE_EPOCH=1700000000 \
        "$PACKER" package >/dev/null
}

OUT_ONE="$TEMPORARY/out-one"
OUT_TWO="$TEMPORARY/out-two"
run_pack linux x86_64 "$OUT_ONE"
run_pack linux x86_64 "$OUT_TWO"
ARCHIVE="anolisa-$VERSION-linux-x86_64.tar.gz"
cmp "$OUT_ONE/$ARCHIVE" "$OUT_TWO/$ARCHIVE"
test "$(tar -tzf "$OUT_ONE/$ARCHIVE" | sed '/^\.\/$/d')" = "./anolisa"
test "$(tar -tvzf "$OUT_ONE/$ARCHIVE" | awk '$NF == "./anolisa" { print $1 }')" = \
    "-rwxr-xr-x"

run_pack linux aarch64 "$TEMPORARY/out-linux-arm64"
test -f "$TEMPORARY/out-linux-arm64/anolisa-$VERSION-linux-aarch64.tar.gz"
run_pack macos aarch64 "$TEMPORARY/out-macos-arm64"
test -f "$TEMPORARY/out-macos-arm64/anolisa-$VERSION-macos-aarch64.tar.gz"

if run_pack macos x86_64 "$TEMPORARY/out-unsupported" 2>/dev/null; then
    printf 'ERROR: unsupported macOS x86_64 package succeeded\n' >&2
    exit 1
fi

BAD_METADATA="$TEMPORARY/bad-sha.toml"
write_metadata linux x86_64 "$BAD_METADATA"
sed -i "s/$DIGEST/$(printf '%064d' 0)/" "$BAD_METADATA"
if ANOLISA_SOURCE_DIR="$ROOT" \
    ANOLISA_BUILD_METADATA="$BAD_METADATA" \
    BIN_DIR="$BIN_DIR" \
    OUTPUT_DIR="$TEMPORARY/out-bad-sha" \
    TARGET_OS=linux \
    TARGET_ARCH=x86_64 \
    SOURCE_DATE_EPOCH=1700000000 \
        "$PACKER" package >/dev/null 2>&1; then
    printf 'ERROR: mismatched binary checksum was accepted\n' >&2
    exit 1
fi

DRIFT_ROOT="$TEMPORARY/version-drift"
install -d -m 0755 "$DRIFT_ROOT/npm/platforms"
install -p -m 0644 "$ROOT/Cargo.toml" "$DRIFT_ROOT/Cargo.toml"
cp -a "$ROOT/npm/package.json" "$DRIFT_ROOT/npm/package.json"
cp -a "$ROOT/npm/platforms/." "$DRIFT_ROOT/npm/platforms/"
sed -i '0,/"version":/s/"version": "[^"]*"/"version": "9.9.9"/' \
    "$DRIFT_ROOT/npm/platforms/linux-x64/package.json"
if python3 "$VERIFIER" "$DRIFT_ROOT" --os linux --arch x86_64 \
    >/dev/null 2>&1; then
    printf 'ERROR: npm version drift was accepted\n' >&2
    exit 1
fi

printf 'ANOLISA CLI prebuilt package tests passed\n'
