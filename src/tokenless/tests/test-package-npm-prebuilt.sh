#!/usr/bin/env bash
# Verify npm packaging consumes, validates, and preserves prebuilt binaries.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d /tmp/tokenless-npm-package-test.XXXXXX)"
PREBUILT="$ROOT/target/npm-prebuilt"
BACKUP="$TMP/npm-prebuilt.backup"
TEST_TOOLS="$TMP/tools"
LEGACY_AGENTSCOPE="$ROOT/adapters/tokenless/agentscope"
LEGACY_AGENTSCOPE_BACKUP="$TMP/legacy-agentscope.backup"

cleanup() {
    rm -rf "$PREBUILT"
    rm -rf "$LEGACY_AGENTSCOPE"
    if [[ -e "$BACKUP" ]]; then
        mv "$BACKUP" "$PREBUILT"
    fi
    if [[ -e "$LEGACY_AGENTSCOPE_BACKUP" ]]; then
        mv "$LEGACY_AGENTSCOPE_BACKUP" "$LEGACY_AGENTSCOPE"
    fi
    rm -rf "$TMP"
}
trap cleanup EXIT

if [[ -e "$PREBUILT" ]]; then
    mv "$PREBUILT" "$BACKUP"
fi
if [[ -e "$LEGACY_AGENTSCOPE" ]]; then
    mv "$LEGACY_AGENTSCOPE" "$LEGACY_AGENTSCOPE_BACKUP"
fi

mkdir -p \
    "$LEGACY_AGENTSCOPE/build/lib/tokenless_agentscope" \
    "$LEGACY_AGENTSCOPE/src/anolisa_tokenless_agentscope.egg-info"
printf '[build-system]\nrequires = ["setuptools"]\n' \
    > "$LEGACY_AGENTSCOPE/pyproject.toml"
printf 'legacy build output\n' \
    > "$LEGACY_AGENTSCOPE/build/lib/tokenless_agentscope/middleware.py"
printf 'Name: anolisa-tokenless-agentscope\n' \
    > "$LEGACY_AGENTSCOPE/src/anolisa_tokenless_agentscope.egg-info/PKG-INFO"

mkdir -p "$TEST_TOOLS"
cat >"$TEST_TOOLS/readelf" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
input="${@: -1}"
grep -aoE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' "$input" || true
SH
chmod 0755 "$TEST_TOOLS/readelf"
export PATH="$TEST_TOOLS:$PATH"

make_binaries() {
    local target="$1"
    local destination="$PREBUILT/$target"

    mkdir -p "$destination"
    python3 - "$target" "$destination" <<'PY'
import pathlib
import struct
import sys

target, destination = sys.argv[1:]
root = pathlib.Path(destination)
if target.startswith("linux-"):
    machine = {"linux-x64": 62, "linux-arm64": 183}[target]
    header = bytearray(64)
    header[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", header, 16, 2)
    struct.pack_into("<H", header, 18, machine)
    struct.pack_into("<I", header, 20, 1)
    content = bytes(header) + b"\nGLIBC_2.17\n"
else:
    cpu = {"darwin-x64": 0x01000007, "darwin-arm64": 0x0100000C}[target]
    content = struct.pack("<IiiIIIII", 0xFEEDFACF, cpu, 0, 2, 0, 0, 0, 0)
for name in ("tokenless", "rtk"):
    (root / name).write_bytes(content + f"\n{name}-{target}\n".encode())
PY
    chmod 0755 "$destination/tokenless" "$destination/rtk"
}

for target in linux-x64 linux-arm64 darwin-x64 darwin-arm64; do
    make_binaries "$target"
done

cp "$PREBUILT/linux-x64/"* "$PREBUILT/linux-arm64/"
if node "$ROOT/npm/scripts/package-npm.js" --target linux-arm64 \
    >"$TMP/mismatch.out" 2>&1; then
    echo "ERROR: npm packer accepted mislabeled x86_64 binaries as arm64" >&2
    exit 1
fi
grep -Fq 'does not match linux-arm64' "$TMP/mismatch.out"
make_binaries linux-arm64

python3 - "$PREBUILT/linux-x64/tokenless" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
path.write_bytes(path.read_bytes().replace(b"GLIBC_2.17", b"GLIBC_2.18"))
PY
if ! node "$ROOT/npm/scripts/package-npm.js" --target linux-x64 \
    >"$TMP/glibc.out" 2>&1; then
    echo "ERROR: npm packer rejected a binary above the GLIBC 2.17 baseline" >&2
    exit 1
fi
grep -Fq 'requires GLIBC_2.18' "$TMP/glibc.out"
make_binaries linux-x64

check_selector() {
    local selector="$1"
    local expected="$2"
    node "$ROOT/npm/scripts/package-npm.js" --target "$selector" >"$TMP/selector.out"
    grep -Fq "Targets: $expected" "$TMP/selector.out"
}

check_selector x86_64 'linux-x64, darwin-x64'
check_selector arm64 'linux-arm64, darwin-arm64'
check_selector linux 'linux-x64, linux-arm64'
check_selector aarch64-apple-darwin 'darwin-arm64'

node "$ROOT/npm/scripts/package-npm.js" --all
test ! -e "$ROOT/npm/dist/tokenless/adapters/tokenless/agentscope"

# Inspect the published tarball: staging can contain working symlinks that
# npm pack silently omits, leaving an installed Agent plugin without hooks.
mkdir -p "$TMP/npm-root"
tar -xzpf "$ROOT/npm/dist/tokenless/"*.tgz -C "$TMP/npm-root"
for agent in claude-code qwencode; do
    dispatcher="$TMP/npm-root/package/adapters/tokenless/$agent/hooks/run-hook.sh"
    test -f "$dispatcher"
    test ! -L "$dispatcher"
    cmp "$ROOT/adapters/tokenless/common/hooks/run-hook.sh" "$dispatcher"
    test "$(stat -c '%a' "$dispatcher")" = 755
    bash -n "$dispatcher"
done

for target in linux-x64 linux-arm64 darwin-x64 darwin-arm64; do
    package="$ROOT/npm/dist/tokenless-$target"
    test -f "$package/package.json"
    test -f "$package"/*.tgz
    for binary in tokenless rtk; do
        cmp "$PREBUILT/$target/$binary" "$package/bin/$binary"
        test "$(stat -c '%a' "$package/bin/$binary")" = 755
    done
done

# Platform packages must not declare bin entries: they would collide with
# the root package's tokenless/rtk bins, and npm resolves such
# collisions by removing every conflicting .bin link, leaving installs
# without a tokenless executable. The root package's postinstall links its
# bins to these platform binaries instead (esbuild's platform packages use
# the same model).
for target in linux-x64 linux-arm64 darwin-x64 darwin-arm64; do
    node - "$ROOT/npm/dist/tokenless-$target/package.json" <<'JS'
const fs = require('node:fs');
const manifest = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (manifest.bin !== undefined) {
  throw new Error(
    `${manifest.name} must not declare bin entries; ` +
    'the root package owns the tokenless/rtk bin names',
  );
}
JS
done

node - "$ROOT/npm/dist/tokenless/package.json" <<'JS'
const fs = require('node:fs');
const manifest = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const expected = [
  '@anolisa/tokenless-linux-x64',
  '@anolisa/tokenless-linux-arm64',
  '@anolisa/tokenless-darwin-x64',
  '@anolisa/tokenless-darwin-arm64',
];
for (const name of expected) {
  if (manifest.optionalDependencies?.[name] !== manifest.version) {
    throw new Error(`missing optional dependency ${name}`);
  }
}
JS

test -f "$ROOT/npm/dist/tokenless/adapters/tokenless/dsh/package.json"
test -f "$ROOT/npm/dist/tokenless/adapters/tokenless/dsh/cordis.patch.yml"
test -f "$ROOT/npm/dist/tokenless/adapters/tokenless/dsh/dist/index.js"
node - "$ROOT/npm/dist/tokenless/adapters/tokenless/dsh/package.json" <<'JS'
const fs = require('node:fs');
const manifest = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (manifest.name !== '@anolisa/dsh-tokenless') throw new Error('wrong dsh package name');
if (manifest.dsh?.bundle?.patch !== './cordis.patch.yml') {
  throw new Error('dsh bundle patch contract missing');
}
JS

if grep -Eq \
    'cargo-zigbuild|cargo zigbuild|cross build|rustup target|SDKROOT|detectBuilder' \
    "$ROOT/npm/scripts/package-npm.js"; then
    echo "ERROR: npm packer still contains cross-compilation logic" >&2
    exit 1
fi
echo "tokenless prebuilt npm package tests passed"
