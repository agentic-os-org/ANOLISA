#!/usr/bin/env bash
# Exercise the component-owned raw packer without compiling native binaries.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d /tmp/tokenless-raw-package-test.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

SOURCE="$TMP/tokenless"
ADAPTERS="$SOURCE/adapters/tokenless"
CONTRACT="$SOURCE/target/packaging/anolisa/component.toml"
# The version this fixture publishes. `make test-raw-package` runs the suite a
# second time with a prerelease so the packer is exercised on the one version
# shape whose two spellings differ (see WHEEL_VERSION below).
VERSION="${TOKENLESS_RAW_TEST_VERSION:-9.8.7}"
# Wheel filenames carry the PEP 440 normalized spelling of the workspace
# version while release-tag paths keep the raw Cargo one. Ask the same helper
# the stamping rule asks, so this fixture mirrors the producer instead of
# restating the rule.
WHEEL_VERSION="$(python3 "$ROOT/packaging/raw/wheel-version.py" "$VERSION")"

mkdir -p \
    "$SOURCE/target/packaging/anolisa" \
    "$ADAPTERS/common/hooks/__pycache__" \
    "$ADAPTERS/common/commands" \
    "$ADAPTERS/qwenwork/.qoder-plugin" \
    "$ADAPTERS/qwenwork/hooks" \
    "$ADAPTERS/qwenwork/scripts/__pycache__" \
    "$ADAPTERS/openclaw/dist" \
    "$ADAPTERS/dsh/dist" \
    "$ADAPTERS/hermes" \
    "$ADAPTERS/qoder/.qoder-plugin" \
    "$ADAPTERS/claude-code/.claude-plugin" \
    "$ADAPTERS/claude-code/hooks" \
    "$ADAPTERS/codex/.codex-plugin" \
    "$ADAPTERS/opencode/scripts" \
    "$ADAPTERS/cmd/scripts" \
    "$ADAPTERS/agentscope/build/lib/tokenless_agentscope" \
    "$ADAPTERS/agentscope/src/anolisa_tokenless_agentscope.egg-info" \
    "$ADAPTERS/qwencode/hooks" \
    "$ADAPTERS/qwenpaw"

cat > "$SOURCE/Cargo.toml" <<EOF
[workspace]
[workspace.package]
version = "$VERSION"
EOF
cat > "$CONTRACT" <<EOF
[component]
name = "tokenless"
version = "$VERSION"

[[adapters]]
framework = "claude-code"
adapter_type = "plugin"
plugin_id = "tokenless"
source = "adapters/claude-code"
dest = "{datadir}/adapters/{component}/claude-code/"

[[adapters]]
framework = "opencode"
adapter_type = "plugin"
plugin_id = "tokenless"
source = "adapters/opencode"
dest = "{datadir}/adapters/{component}/opencode/"

[[adapters]]
framework = "qwenwork"
adapter_type = "plugin"
plugin_id = "tokenless"
source = "adapters/qwenwork"
dest = "{datadir}/adapters/{component}/qwenwork/"

[adapters.bundle]
entry = ".qoder-plugin/plugin.json"
EOF

# A contract that drops one shipped adapter must fail before publication.
python3 - "$CONTRACT" "$TMP" <<'PY'
from pathlib import Path
import sys

contract, output = map(Path, sys.argv[1:])
header, *blocks = contract.read_text().split("[[adapters]]")
for framework in ("opencode", "qwenwork"):
    kept = [b for b in blocks if f'framework = "{framework}"' not in b]
    (output / f"contract-missing-{framework}.toml").write_text(
        header + "".join("[[adapters]]" + b for b in kept)
    )
PY

write_json_version() {
    mkdir -p "$(dirname "$1")"
    printf '{"name":"tokenless","version":"%s"}\n' "$VERSION" > "$1"
}

printf '{"component":"tokenless","version":"%s","targets":{"claude-code":{},"opencode":{},"qwenwork":{}}}\n' \
    "$VERSION" > "$ADAPTERS/manifest.json"
cp "$ROOT/adapters/tokenless/qwenwork/.qoder-plugin/plugin.json.in" \
    "$ADAPTERS/qwenwork/.qoder-plugin/plugin.json.in"
sed "s/@VERSION@/$VERSION/g" "$ADAPTERS/qwenwork/.qoder-plugin/plugin.json.in" \
    > "$ADAPTERS/qwenwork/.qoder-plugin/plugin.json"
printf '{}\n' > "$ADAPTERS/qwenwork/hooks/hooks.json"
printf '#!/usr/bin/env bash\nexit 0\n' > "$ADAPTERS/qwenwork/hooks/run-hook.sh"
printf '# Lifecycle fixture\n' > "$ADAPTERS/qwenwork/scripts/lifecycle.py"
for action in detect install uninstall; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$ADAPTERS/qwenwork/scripts/$action.sh"
done
for hook in hook_utils rewrite_hook compress_response_hook; do
    printf '# Shared hook fixture\n' > "$ADAPTERS/common/hooks/$hook.py"
done
printf '{}\n' > "$ADAPTERS/common/hooks/tool_categories.json"
printf 'stale bytecode\n' > "$ADAPTERS/common/hooks/__pycache__/hook_utils.cpython-312.pyc"
printf 'stale bytecode\n' > "$ADAPTERS/common/hooks/legacy.pyc"
printf 'stale bytecode\n' > "$ADAPTERS/common/hooks/legacy.pyo"
printf 'stale bytecode\n' > "$ADAPTERS/qwenwork/scripts/__pycache__/lifecycle.cpython-312.pyc"
ln -s ../../common/hooks "$ADAPTERS/qwenwork/hooks/shared"
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
printf 'export const Tokenless = {};\n' > "$ADAPTERS/opencode/plugin.js"
for action in detect install uninstall; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$ADAPTERS/opencode/scripts/$action.sh"
done
printf 'export default function tokenless() {}\n' > "$ADAPTERS/cmd/tokenless.ts"
for action in detect install uninstall; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$ADAPTERS/cmd/scripts/$action.sh"
done
printf 'anolisa-tokenless @ https://github.com/alibaba/anolisa/releases/download/tokenless/v%s/anolisa_tokenless-%s-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl ; sys_platform == "linux"\n' \
    "$VERSION" "$WHEEL_VERSION" > "$ADAPTERS/qwenpaw/requirements.txt"
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
    cpu = {"aarch64": 0x0100000C, "x86_64": 0x01000007}[arch]
    content = struct.pack("<IiiIIIII", 0xFEEDFACF, cpu, 0, 2, 0, 0, 0, 0)
for name in ("tokenless", "rtk"):
    (root / name).write_bytes(content)
PY
    chmod 0755 "$destination/tokenless" "$destination/rtk"
}

LINUX_X64="$TMP/bin-linux-x64"
LINUX_ARM64="$TMP/bin-linux-arm64"
MACOS_ARM64="$TMP/bin-macos-arm64"
MACOS_X64="$TMP/bin-macos-x64"
make_binaries linux x86_64 "$LINUX_X64"
make_binaries linux aarch64 "$LINUX_ARM64"
make_binaries macos aarch64 "$MACOS_ARM64"
make_binaries macos x86_64 "$MACOS_X64"

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
run_pack darwin x64 "$MACOS_X64" "$TMP/out-macos-x64"
run_pack macos x86_64 "$MACOS_X64" "$TMP/out-macos-x64-repeat"
cmp "$TMP/out-macos-x64/tokenless-$VERSION-macos-x86_64.tar.gz" \
    "$TMP/out-macos-x64-repeat/tokenless-$VERSION-macos-x86_64.tar.gz"

if run_pack macos x64 "$LINUX_X64" "$TMP/wrong-format" 2>/dev/null; then
    echo "ERROR: Linux binaries unexpectedly passed as macOS x86_64" >&2
    exit 1
fi
if run_pack macos x64 "$MACOS_ARM64" "$TMP/wrong-macos-arch" 2>/dev/null; then
    echo "ERROR: arm64 binaries unexpectedly passed as macOS x86_64" >&2
    exit 1
fi
if run_pack linux aarch64 "$LINUX_X64" "$TMP/mislabeled" 2>/dev/null; then
    echo "ERROR: mislabeled x86_64 binaries unexpectedly passed as aarch64" >&2
    exit 1
fi
for framework in opencode qwenwork; do
    if TOKENLESS_SOURCE_DIR="$SOURCE" \
        RAW_CONTRACT="$TMP/contract-missing-$framework.toml" \
        BIN_DIR="$LINUX_X64" \
        TARGET_OS=linux \
        TARGET_ARCH=x86_64 \
        OUTPUT_DIR="$TMP/undeclared-$framework" \
        SOURCE_DATE_EPOCH=1700000000 \
        "$ROOT/packaging/raw/package.sh" package >"$TMP/coverage.out" 2>&1; then
        echo "ERROR: a contract omitting $framework unexpectedly passed" >&2
        exit 1
    fi
    grep -Fq "declares no [[adapters]] entry for: $framework" "$TMP/coverage.out"
done

cp "$ADAPTERS/manifest.json" "$TMP/manifest.json"
printf '{"component":"tokenless","version":"%s","targets":{"claude-code":{},"opencode":{}}}\n' \
    "$VERSION" > "$ADAPTERS/manifest.json"
if run_pack linux x64 "$LINUX_X64" "$TMP/missing-target" >"$TMP/coverage.out" 2>&1; then
    echo "ERROR: a manifest omitting qwenwork unexpectedly passed" >&2
    exit 1
fi
grep -Fq 'does not ship: qwenwork' "$TMP/coverage.out"
mv "$TMP/manifest.json" "$ADAPTERS/manifest.json"

cp "$ADAPTERS/qwenwork/.qoder-plugin/plugin.json" "$TMP/qwenwork-plugin.json"
printf '{"version":"0.0.0-stale"}\n' > "$ADAPTERS/qwenwork/.qoder-plugin/plugin.json"
if run_pack linux x64 "$LINUX_X64" "$TMP/stale-plugin" >"$TMP/stale.out" 2>&1; then
    echo "ERROR: a stale QwenWork plugin version unexpectedly passed" >&2
    exit 1
fi
grep -Fq 'qwenwork/.qoder-plugin/plugin.json=0.0.0-stale' "$TMP/stale.out"
mv "$TMP/qwenwork-plugin.json" "$ADAPTERS/qwenwork/.qoder-plugin/plugin.json"

EXTRACTED="$TMP/extracted"
mkdir -p "$EXTRACTED"
tar -xzf "$OUT_ONE/$LINUX_ARTIFACT" -C "$EXTRACTED"
cmp "$CONTRACT" "$EXTRACTED/.anolisa/component.toml"
cmp "$LINUX_X64/tokenless" "$EXTRACTED/bin/tokenless"
cmp "$LINUX_X64/rtk" "$EXTRACTED/libexec/anolisa/tokenless/rtk"

# The archived QwenPaw pin keeps both spellings of the one version it was
# stamped from: the raw workspace version in the release-tag path (the tag the
# release workflow creates verbatim) and the normalized version in the wheel
# filename (the only form pip installs from). A prerelease is the shape where
# the two differ; verify-release.py already refuses a package that mixes them.
ARCHIVED_REQUIREMENTS="$EXTRACTED/adapters/qwenpaw/requirements.txt"
cmp "$ADAPTERS/qwenpaw/requirements.txt" "$ARCHIVED_REQUIREMENTS"
grep -Fq "/tokenless/v$VERSION/" "$ARCHIVED_REQUIREMENTS"
grep -Fq "anolisa_tokenless-$WHEEL_VERSION-cp311-abi3-" "$ARCHIVED_REQUIREMENTS"

python3 - "$VERSION" "$OUT_ONE/$LINUX_ARTIFACT" \
    "$TMP/out-linux-arm64/tokenless-$VERSION-linux-aarch64.tar.gz" \
    "$TMP/out-macos-arm64/tokenless-$VERSION-macos-aarch64.tar.gz" \
    "$TMP/out-macos-x64/tokenless-$VERSION-macos-x86_64.tar.gz" <<'PY'
import json
from pathlib import PurePosixPath
import sys
import tarfile

version, *archives = sys.argv[1:]
required = (
    ".qoder-plugin/plugin.json", "hooks/hooks.json", "hooks/run-hook.sh",
    "scripts/detect.sh", "scripts/install.sh", "scripts/uninstall.sh",
    "scripts/lifecycle.py", "hooks/shared/hook_utils.py",
    "hooks/shared/rewrite_hook.py", "hooks/shared/compress_response_hook.py",
    "hooks/shared/tool_categories.json",
)
for archive in archives:
    with tarfile.open(archive) as tar:
        members = {m.name.removeprefix("./"): m for m in tar.getmembers()}
        for name, member in members.items():
            assert not member.issym() and not member.islnk(), (archive, name)
            assert "__pycache__" not in PurePosixPath(name).parts, (archive, name)
            assert not name.endswith((".pyc", ".pyo", ".in")), (archive, name)
        assert members["adapters/qwenwork/hooks/shared"].isdir()
        for relative in required:
            assert members[f"adapters/qwenwork/{relative}"].isfile(), relative
        for name, member in members.items():
            if name.startswith("adapters/common/hooks/") and member.isfile():
                shared = name.replace("adapters/common/hooks/", "adapters/qwenwork/hooks/shared/", 1)
                assert tar.extractfile(member).read() == tar.extractfile(members[shared]).read(), shared
        for name in ("adapters/manifest.json", "adapters/qwenwork/.qoder-plugin/plugin.json"):
            document = json.load(tar.extractfile(members[name]))
            assert document["version"] == version, (archive, name)
            if name.endswith("/manifest.json"):
                assert "qwenwork" in document["targets"]
PY

for relative in \
    adapters/claude-code/hooks/run-hook.sh \
    adapters/qwencode/hooks/run-hook.sh; do
    test -f "$EXTRACTED/$relative"
    test ! -L "$EXTRACTED/$relative"
    cmp "$ADAPTERS/common/hooks/run-hook.sh" "$EXTRACTED/$relative"
done
for relative in \
    adapters/opencode/plugin.js \
    adapters/opencode/scripts/detect.sh \
    adapters/opencode/scripts/install.sh \
    adapters/opencode/scripts/uninstall.sh \
    adapters/cmd/tokenless.ts \
    adapters/cmd/scripts/detect.sh \
    adapters/cmd/scripts/install.sh \
    adapters/cmd/scripts/uninstall.sh; do
    test -f "$EXTRACTED/$relative"
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
test "$(stat -c '%a' "$EXTRACTED/adapters/opencode/plugin.js")" = 644
test "$(stat -c '%a' "$EXTRACTED/adapters/opencode/scripts/install.sh")" = 755
test "$(stat -c '%a' "$EXTRACTED/adapters/cmd/tokenless.ts")" = 644
test "$(stat -c '%a' "$EXTRACTED/adapters/cmd/scripts/install.sh")" = 755

grep -Fq 'source = "bin/tokenless"' "$ROOT/packaging/anolisa/component.toml.in"
grep -Fq 'source = "extensions/tokenless"' "$ROOT/packaging/anolisa/component.toml.in"
test "$(grep -o '@VERSION@' "$ROOT/packaging/anolisa/component.toml.in" | wc -l)" = 1
grep -Fq 'framework = "opencode"' "$ROOT/packaging/anolisa/component.toml.in"
grep -Fq 'framework = "cmd"' "$ROOT/packaging/anolisa/component.toml.in"
CMD_CONTRACT="$(sed -n '/^framework = "cmd"$/,$p' "$ROOT/packaging/anolisa/component.toml.in")"
grep -Fq 'when = "post_enable"' <<< "$CMD_CONTRACT"
grep -Fq 'when = "post_disable"' <<< "$CMD_CONTRACT"
OPENCODE_CONTRACT="$(sed -n '/^framework = "opencode"$/,/^\[\[adapters\]\]$/p' "$ROOT/packaging/anolisa/component.toml.in")"
grep -Fq 'when = "post_enable"' <<< "$OPENCODE_CONTRACT"
grep -Fq 'when = "post_disable"' <<< "$OPENCODE_CONTRACT"
grep -Fq 'source = "adapters/manifest.json"' "$ROOT/packaging/anolisa/component.toml.in"

# Every adapter target the manifest ships needs a matching [[adapters]] entry,
# otherwise the raw install silently drops that adapter. verify-release.py
# enforces this while packaging; assert it on the shipped sources as well.
python3 - "$ROOT/adapters/tokenless/manifest.json.in" \
    "$ROOT/packaging/anolisa/component.toml.in" \
    "$ROOT/../../distribution/anolisa/manifests/components/tokenless/component.toml" <<'DRIFT_PY'
import json
import re
import sys

manifest_path, contract_path, seed_path = sys.argv[1:]
with open(manifest_path, encoding="utf-8") as handle:
    targets = set(json.load(handle).get("targets", {}))
assert "qwenwork" in targets
with open(contract_path, encoding="utf-8") as handle:
    contract_text = handle.read()
blocks = re.findall(r"(?ms)^\[\[adapters\]\]\s*$.*?(?=^\[|\Z)", contract_text)
frameworks = {re.search(r'(?m)^framework\s*=\s*"([^"]+)"', b).group(1) for b in blocks}
undeclared = sorted(targets - frameworks)
unshipped = sorted(frameworks - targets)
if undeclared or unshipped:
    sys.exit(
        "ERROR: adapter contract drift: undeclared={} unshipped={}".format(undeclared, unshipped)
    )
expected = {
    "framework": "qwenwork", "adapter_type": "plugin", "plugin_id": "tokenless",
    "source": "adapters/qwenwork", "dest": "{datadir}/adapters/{component}/qwenwork/",
    "entry": ".qoder-plugin/plugin.json",
}
for path in (contract_path, seed_path):
    with open(path, encoding="utf-8") as handle:
        blocks = handle.read().split("[[adapters]]")[1:]
    matches = [b for b in blocks if re.search(r'^framework\s*=\s*"qwenwork"$', b, re.M)]
    assert len(matches) == 1, path
    for key, value in expected.items():
        match = re.search(r'^' + key + r'\s*=\s*"([^"]+)"$', matches[0], re.M)
        assert match and match.group(1) == value, (path, key)
DRIFT_PY

echo "tokenless component-owned raw package tests passed"
