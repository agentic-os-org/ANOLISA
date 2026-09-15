#!/usr/bin/env bash
# Exercise the component-owned raw packer without compiling native binaries.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d /tmp/tokenless-raw-package-test.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

SOURCE="$TMP/tokenless"
ADAPTERS="$SOURCE/adapters/tokenless"
CONTRACT="$SOURCE/.anolisa/component.toml"
VERSION="9.8.7"

mkdir -p \
    "$SOURCE/.anolisa" \
    "$ADAPTERS/common/hooks" \
    "$ADAPTERS/common/commands" \
    "$ADAPTERS/openclaw/dist" \
    "$ADAPTERS/dsh/dist" \
    "$ADAPTERS/hermes" \
    "$ADAPTERS/qoder/.qoder-plugin" \
    "$ADAPTERS/claude-code/.claude-plugin" \
    "$ADAPTERS/claude-code/hooks" \
    "$ADAPTERS/codex/.codex-plugin" \
    "$ADAPTERS/agentscope/build/lib/tokenless_agentscope" \
    "$ADAPTERS/agentscope/src/anolisa_tokenless_agentscope.egg-info" \
    "$ADAPTERS/qwencode/hooks" \
    "$ADAPTERS/qwenpaw" \
    "$ADAPTERS/trae/hooks"

cat > "$SOURCE/Cargo.toml" <<EOF
[workspace]
[workspace.package]
version = "$VERSION"
EOF
cat > "$CONTRACT" <<EOF
[component]
name = "tokenless"
version = "$VERSION"
EOF

write_json_version() {
    mkdir -p "$(dirname "$1")"
    printf '{"name":"tokenless","version":"%s"}\n' "$VERSION" > "$1"
}

write_json_version "$ADAPTERS/manifest.json"
write_json_version "$ADAPTERS/openclaw/package.json"
write_json_version "$ADAPTERS/openclaw/openclaw.plugin.json"
printf '{"lockfileVersion":3}\n' > "$ADAPTERS/openclaw/package-lock.json"
write_json_version "$ADAPTERS/dsh/package.json"
write_json_version "$ADAPTERS/qoder/.qoder-plugin/plugin.json"
write_json_version "$ADAPTERS/claude-code/.claude-plugin/plugin.json"
write_json_version "$ADAPTERS/codex/.codex-plugin/plugin.json"
write_json_version "$ADAPTERS/qwencode/qwen-extension.json"
write_json_version "$ADAPTERS/qwenpaw/plugin.json"
printf 'plugin = None\n' > "$ADAPTERS/qwenpaw/plugin.py"
printf 'anolisa-tokenless @ https://github.com/alibaba/anolisa/releases/download/tokenless/v%s/anolisa_tokenless-%s-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl ; sys_platform == "linux"\n' \
    "$VERSION" "$VERSION" > "$ADAPTERS/qwenpaw/requirements.txt"
printf '{"name":"anolisa-tokenless"}\n' \
    > "$ADAPTERS/claude-code/.claude-plugin/marketplace.json"
printf 'version: "%s"\n' "$VERSION" > "$ADAPTERS/hermes/plugin.yaml"
printf 'export default {};\n' > "$ADAPTERS/openclaw/dist/index.js"
printf '%s\n' '- insert:' '    - id: anolisa-tokenless' "      name: '@anolisa/dsh-tokenless'" \
    > "$ADAPTERS/dsh/cordis.patch.yml"
printf 'export function apply() {}\n' > "$ADAPTERS/dsh/dist/index.js"
printf '{"name":"tokenless","version":"%s"}\n' "$VERSION" \
    > "$ADAPTERS/common/cosh-extension.json"
printf '{}\n' > "$ADAPTERS/common/tool-ready-spec.json"
printf '#!/usr/bin/env bash\nexit 0\n' > "$ADAPTERS/common/tokenless-env-fix.sh"
printf '#!/usr/bin/env bash\nprintf "shared hook\\n"\n' \
    > "$ADAPTERS/common/hooks/run-hook.sh"
printf 'description = "fixture"\n' \
    > "$ADAPTERS/common/commands/tokenless-stats.toml"
printf '[build-system]\nrequires = ["setuptools"]\n' \
    > "$ADAPTERS/agentscope/pyproject.toml"
printf 'legacy build output\n' \
    > "$ADAPTERS/agentscope/build/lib/tokenless_agentscope/middleware.py"
printf 'Name: anolisa-tokenless-agentscope\n' \
    > "$ADAPTERS/agentscope/src/anolisa_tokenless_agentscope.egg-info/PKG-INFO"
chmod 0755 \
    "$ADAPTERS/common/tokenless-env-fix.sh" \
    "$ADAPTERS/common/hooks/run-hook.sh"
ln -s ../../common/hooks/run-hook.sh "$ADAPTERS/claude-code/hooks/run-hook.sh"
ln -s ../../common/hooks/run-hook.sh "$ADAPTERS/qwencode/hooks/run-hook.sh"
ln -s ../../common/hooks/run-hook.sh "$ADAPTERS/trae/hooks/run-hook.sh"

make_binaries() {
    local os="$1"
    local arch="$2"
    local destination="$3"

    mkdir -p "$destination"
    python3 - "$os" "$arch" "$destination" <<'PY'
import pathlib
import struct
import sys

os_name, arch, destination = sys.argv[1:]
root = pathlib.Path(destination)
if os_name == "linux":
    machine = {"x86_64": 62, "aarch64": 183}[arch]
    header = bytearray(64)
    header[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", header, 16, 2)
    struct.pack_into("<H", header, 18, machine)
    struct.pack_into("<I", header, 20, 1)
    content = bytes(header)
else:
    cpu = {"aarch64": 0x0100000C}[arch]
    content = struct.pack("<IiiIIIII", 0xFEEDFACF, cpu, 0, 2, 0, 0, 0, 0)
for name in ("tokenless", "rtk"):
    (root / name).write_bytes(content)
PY
    chmod 0755 "$destination/tokenless" "$destination/rtk"
}

LINUX_X64="$TMP/bin-linux-x64"
LINUX_ARM64="$TMP/bin-linux-arm64"
MACOS_ARM64="$TMP/bin-macos-arm64"
make_binaries linux x86_64 "$LINUX_X64"
make_binaries linux aarch64 "$LINUX_ARM64"
make_binaries macos aarch64 "$MACOS_ARM64"

run_pack() {
    local os="$1"
    local arch="$2"
    local bins="$3"
    local output="$4"

    TOKENLESS_SOURCE_DIR="$SOURCE" \
    RAW_CONTRACT="$CONTRACT" \
    BIN_DIR="$bins" \
    TARGET_OS="$os" \
    TARGET_ARCH="$arch" \
    OUTPUT_DIR="$output" \
    SOURCE_DATE_EPOCH=1700000000 \
        "$ROOT/packaging/raw/package.sh" package >/dev/null
}

OUT_ONE="$TMP/out-one"
OUT_TWO="$TMP/out-two"
run_pack linux x64 "$LINUX_X64" "$OUT_ONE"
run_pack linux x86_64 "$LINUX_X64" "$OUT_TWO"
LINUX_ARTIFACT="tokenless-$VERSION-linux-x86_64.tar.gz"
cmp "$OUT_ONE/$LINUX_ARTIFACT" "$OUT_TWO/$LINUX_ARTIFACT"

run_pack linux arm64 "$LINUX_ARM64" "$TMP/out-linux-arm64"
test -f "$TMP/out-linux-arm64/tokenless-$VERSION-linux-aarch64.tar.gz"
run_pack darwin arm64 "$MACOS_ARM64" "$TMP/out-macos-arm64"
test -f "$TMP/out-macos-arm64/tokenless-$VERSION-macos-aarch64.tar.gz"

if run_pack macos x64 "$LINUX_X64" "$TMP/unsupported" 2>/dev/null; then
    echo "ERROR: macOS x86_64 raw packaging unexpectedly succeeded" >&2
    exit 1
fi
if run_pack linux aarch64 "$LINUX_X64" "$TMP/mislabeled" 2>/dev/null; then
    echo "ERROR: mislabeled x86_64 binaries unexpectedly passed as aarch64" >&2
    exit 1
fi

EXTRACTED="$TMP/extracted"
mkdir -p "$EXTRACTED"
tar -xzf "$OUT_ONE/$LINUX_ARTIFACT" -C "$EXTRACTED"
cmp "$CONTRACT" "$EXTRACTED/.anolisa/component.toml"
cmp "$LINUX_X64/tokenless" "$EXTRACTED/bin/tokenless"
cmp "$LINUX_X64/rtk" "$EXTRACTED/libexec/anolisa/tokenless/rtk"

for relative in \
    adapters/claude-code/hooks/run-hook.sh \
    adapters/qwencode/hooks/run-hook.sh \
    adapters/trae/hooks/run-hook.sh; do
    test -f "$EXTRACTED/$relative"
    test ! -L "$EXTRACTED/$relative"
    cmp "$ADAPTERS/common/hooks/run-hook.sh" "$EXTRACTED/$relative"
done
test -f "$EXTRACTED/adapters/dsh/package.json"
test -f "$EXTRACTED/adapters/dsh/cordis.patch.yml"
test -f "$EXTRACTED/adapters/dsh/dist/index.js"
test -f "$EXTRACTED/extensions/tokenless/cosh-extension.json"
test -f "$EXTRACTED/extensions/tokenless/hooks/run-hook.sh"
test ! -e "$EXTRACTED/adapters/agentscope"
test -z "$(find "$EXTRACTED" -type l -print -quit)"
test -z "$(find "$EXTRACTED" \( \
    -name '*.in' -o \
    -name package-lock.json -o \
    -name node_modules -o \
    -name build -o \
    -name '*.egg-info' -o \
    -name '__pycache__' -o \
    -name '*.pyc' -o \
    -name '*.pyo' \
\) -print -quit)"
test "$(stat -c '%a' "$EXTRACTED/bin/tokenless")" = 755
test "$(stat -c '%a' "$EXTRACTED/adapters/manifest.json")" = 644
test "$(stat -c '%a' "$EXTRACTED/adapters/common/hooks/run-hook.sh")" = 755

grep -Fq 'source = "bin/tokenless"' "$ROOT/.anolisa/component.toml.in"
grep -Fq 'source = "extensions/tokenless"' "$ROOT/.anolisa/component.toml.in"
test "$(grep -o '@VERSION@' "$ROOT/.anolisa/component.toml.in" | wc -l)" = 1

echo "tokenless component-owned raw package tests passed"
