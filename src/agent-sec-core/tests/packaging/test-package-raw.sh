#!/usr/bin/env bash
# Fixture-driven regression tests for component-owned raw packaging.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

BUILD="$TMP/build"
VERSION="$(python3 "$ROOT/packaging/raw/verify_release.py" \
    "$ROOT" "$ROOT/.anolisa/component.toml")"
ASSET_VERIFY_SOURCE_CONFIG="$ROOT/agent-sec-cli/src/agent_sec_cli/asset_verify/config.conf"
ASSET_VERIFY_PACKAGE_PATH="lib/anolisa/sec-core/python3.11/site-packages/agent_sec_cli/asset_verify/config.conf"

assert_asset_verify_config() {
    local config="$1"

    cmp "$ASSET_VERIFY_SOURCE_CONFIG" "$config"
    test "$(grep -Fc '/usr/share/anolisa/skills' "$config")" = "1"
    test "$(grep -Fc '/usr/local/share/anolisa/skills' "$config")" = "1"
}

install -d -m 0755 \
    "$BUILD/site-packages/agent_sec_cli/asset_verify" \
    "$BUILD/site-packages/agent_sec_cli/daemon" \
    "$BUILD/site-packages/agent_sec_cli/__pycache__" \
    "$BUILD/python-runtime/bin" \
    "$BUILD/python-runtime/lib/Tix8.4.3" \
    "$BUILD/python-runtime/lib/python3.11/__pycache__" \
    "$BUILD/python-runtime/lib/tk8.6/demos" \
    "$BUILD/openclaw-plugin/dist" \
    "$BUILD/openclaw-plugin/scripts" \
    "$BUILD/hermes-plugin/src" \
    "$BUILD/hermes-plugin/scripts" \
    "$BUILD/codex-plugin/hooks-plugin/.codex-plugin" \
    "$BUILD/codex-plugin/hooks-plugin/hooks" \
    "$BUILD/qoder-plugin/.qoder-plugin" \
    "$BUILD/qoder-plugin/hooks" \
    "$BUILD/qwen-code-extension/hooks" \
    "$BUILD/cosh-extension/hooks"

printf '__version__ = "%s"\n' "$VERSION" > \
    "$BUILD/site-packages/agent_sec_cli/__init__.py"
printf 'def main():\n    return 0\n' > \
    "$BUILD/site-packages/agent_sec_cli/cli.py"
printf 'def main():\n    return 0\n' > \
    "$BUILD/site-packages/agent_sec_cli/daemon/server.py"
cp "$ASSET_VERIFY_SOURCE_CONFIG" \
    "$BUILD/site-packages/agent_sec_cli/asset_verify/config.conf"
printf 'host-specific bytecode\n' > \
    "$BUILD/site-packages/agent_sec_cli/__pycache__/cli.cpython-311.pyc"

cat > "$BUILD/python-runtime/bin/python3.11.real" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "-c" ]; then
    case "${2:-}" in
        *platform.python_version*)
            [ -z "${PYTHONDONTWRITEBYTECODE:-}" ] || exit 9
            printf '3.11.6\n'
            exit 0
            ;;
    esac
fi
if [ -n "${ANOLISA_TEST_PYTHON_LOG:-}" ]; then
    printf '%s\n' "$0" > "$ANOLISA_TEST_PYTHON_LOG"
    printf '%s\n' "${PYTHONHOME:-}" > "$ANOLISA_TEST_PYTHON_LOG.home"
    printf '%s\n' "${PYTHONPATH:-}" > "$ANOLISA_TEST_PYTHON_LOG.path"
fi
EOF
chmod 0755 "$BUILD/python-runtime/bin/python3.11.real"
ln -s python3.11.real "$BUILD/python-runtime/bin/python3.11"
printf 'fixture Python license\n' > \
    "$BUILD/python-runtime/lib/python3.11/LICENSE.txt"
printf 'fixture Tix license\n' > \
    "$BUILD/python-runtime/lib/Tix8.4.3/license.terms"
printf 'fixture Tk license\n' > \
    "$BUILD/python-runtime/lib/tk8.6/demos/license.terms"
printf 'host-specific runtime bytecode\n' > \
    "$BUILD/python-runtime/lib/python3.11/__pycache__/site.cpython-311.pyc"
printf 'fixture libpython\n' > \
    "$BUILD/python-runtime/lib/libpython3.11.so.1.0"
ln -s libpython3.11.so.1.0 \
    "$BUILD/python-runtime/lib/libpython3.11.so"
cp "$ROOT/packaging/raw/assets/python-runtime/PROVENANCE.toml" \
    "$BUILD/python-runtime/PROVENANCE.toml"
(
    cd "$BUILD/python-runtime"
    sha256sum \
        lib/Tix8.4.3/license.terms \
        lib/python3.11/LICENSE.txt \
        lib/tk8.6/demos/license.terms > LICENSES.sha256
)

printf '#!/bin/sh\necho linux-sandbox fixture\n' > "$BUILD/linux-sandbox"
chmod 0755 "$BUILD/linux-sandbox"

cp "$ROOT/openclaw-plugin/openclaw.plugin.json" "$BUILD/openclaw-plugin/"
cp "$ROOT/openclaw-plugin/package.json" "$BUILD/openclaw-plugin/"
printf 'export default {};\n' > "$BUILD/openclaw-plugin/dist/index.js"
printf '#!/bin/sh\n' > "$BUILD/openclaw-plugin/scripts/deploy.sh"

cp "$ROOT/hermes-plugin/src/plugin.yaml" "$BUILD/hermes-plugin/src/"
printf 'def register(ctx):\n    return None\n' > "$BUILD/hermes-plugin/src/__init__.py"
printf '#!/bin/sh\n' > "$BUILD/hermes-plugin/scripts/deploy.sh"

cp "$ROOT/codex-plugin/hooks-plugin/.codex-plugin/plugin.json" \
    "$BUILD/codex-plugin/hooks-plugin/.codex-plugin/"
cp "$ROOT/codex-plugin/hooks-plugin/hooks/hooks.json" \
    "$BUILD/codex-plugin/hooks-plugin/hooks/"
printf 'print("fixture")\n' > "$BUILD/codex-plugin/hooks-plugin/hooks/hook.py"

cp "$ROOT/qoder-plugin/.qoder-plugin/plugin.json" \
    "$BUILD/qoder-plugin/.qoder-plugin/"
cp "$ROOT/qoder-plugin/hooks/hooks.json" "$BUILD/qoder-plugin/hooks/"
printf 'print("fixture")\n' > "$BUILD/qoder-plugin/hooks/hook.py"

cp "$ROOT/qwen-code-extension/qwen-extension.json" \
    "$BUILD/qwen-code-extension/"
printf 'print("fixture")\n' > "$BUILD/qwen-code-extension/hooks/hook.py"

cp "$ROOT/cosh-extension/cosh-extension.json" "$BUILD/cosh-extension/"
printf 'print("fixture")\n' > "$BUILD/cosh-extension/hooks/hook.py"

make -C "$ROOT" stage-skills BUILD_DIR="$BUILD"

run_package() {
    local output="$1"

    BUILD_DIR="$BUILD" \
    OUTPUT_DIR="$output" \
    TARGET_OS=linux \
    TARGET_ARCH=x86_64 \
    SOURCE_DATE_EPOCH=1783656696 \
        "$ROOT/packaging/raw/package.sh" package
}

OUT_ONE="$TMP/out-one"
OUT_TWO="$TMP/out-two"
run_package "$OUT_ONE"
run_package "$OUT_TWO"

ARTIFACT="sec-core-${VERSION}-linux-x86_64.tar.gz"
cmp "$OUT_ONE/$ARTIFACT" "$OUT_TWO/$ARTIFACT"

STAGE="$TMP/stage"
make -C "$ROOT" stage-raw \
    BUILD_DIR="$BUILD" \
    DESTDIR="$STAGE" \
    TARGET_OS=linux \
    TARGET_ARCH=x86_64
test "$(stat -c '%a' "$STAGE/bin/agent-sec-cli")" = "755"
test "$(stat -c '%a' "$STAGE/bin/agent-sec-python")" = "755"
test "$(stat -c '%a' \
    "$STAGE/lib/anolisa/sec-core/python3.11/runtime/bin/python3.11")" = "755"
test "$(stat -c '%a' "$STAGE/.anolisa/component.toml")" = "644"
test -z "$(find "$STAGE" -type l -print -quit)"
cmp "$ROOT/.anolisa/component.toml" "$STAGE/.anolisa/component.toml"
assert_asset_verify_config "$STAGE/$ASSET_VERIFY_PACKAGE_PATH"

RPM_STAGE="$TMP/rpm-stage"
MANIFEST_STAGE="$TMP/manifest-stage"
make -C "$ROOT" stage-component-manifest BUILD_DIR="$MANIFEST_STAGE"
cmp "$ROOT/.anolisa/component.toml" \
    "$MANIFEST_STAGE/share/anolisa/components/sec-core/component.toml"
make -C "$ROOT" install-component-manifest install-systemd-user \
    DESTDIR="$RPM_STAGE"
make -C "$ROOT" install-skills BUILD_DIR="$BUILD" DESTDIR="$RPM_STAGE"
for skill_dir in "$ROOT"/skills/*; do
    skill="$(basename "$skill_dir")"
    cmp "$skill_dir/SKILL.md" "$STAGE/share/anolisa/skills/$skill/SKILL.md"
    cmp "$skill_dir/SKILL.md" "$RPM_STAGE/usr/share/anolisa/skills/$skill/SKILL.md"
done
cmp "$ROOT/.anolisa/component.toml" \
    "$RPM_STAGE/usr/share/anolisa/components/sec-core/component.toml"
if [ -e "$ROOT/adapters/component.toml" ]; then
    echo "ERROR: legacy RPM-only contract still exists" >&2
    exit 1
fi
grep -Fq 'ExecStart="/usr/bin/agent-sec-daemon" serve' \
    "$RPM_STAGE/usr/lib/systemd/user/agent-sec-core.service"
grep -Fq 'ReadWritePaths="/usr/share/anolisa"' \
    "$RPM_STAGE/usr/lib/systemd/user/agent-sec-core.service"
if grep -Eq '\{(bindir|datadir)\}' \
    "$RPM_STAGE/usr/lib/systemd/user/agent-sec-core.service"; then
    echo "ERROR: RPM service retained layout placeholders" >&2
    exit 1
fi

PYTHON_LOG="$TMP/python.log"
ANOLISA_TEST_PYTHON_LOG="$PYTHON_LOG" "$STAGE/bin/agent-sec-cli" --version
test "$(cat "$PYTHON_LOG")" = \
    "$STAGE/lib/anolisa/sec-core/python3.11/runtime/bin/python3.11"
test "$(cat "$PYTHON_LOG.home")" = \
    "$STAGE/lib/anolisa/sec-core/python3.11/runtime"
test "$(cat "$PYTHON_LOG.path")" = \
    "$STAGE/lib/anolisa/sec-core/python3.11/site-packages"

SOURCE_HOOK_MANIFESTS=(
    "$ROOT/codex-plugin/hooks-plugin/hooks/hooks.json"
    "$ROOT/qoder-plugin/hooks/hooks.json"
    "$ROOT/qwen-code-extension/qwen-extension.json"
    "$ROOT/cosh-extension/cosh-extension.json"
)
RAW_HOOK_MANIFESTS=(
    "$STAGE/adapters/sec-core/codex/hooks/hooks.json"
    "$STAGE/adapters/sec-core/qoder/hooks/hooks.json"
    "$STAGE/adapters/sec-core/qwencode/qwen-extension.json"
    "$STAGE/adapters/sec-core/cosh/cosh-extension.json"
)
for index in "${!RAW_HOOK_MANIFESTS[@]}"; do
    source_manifest="${SOURCE_HOOK_MANIFESTS[$index]}"
    raw_manifest="${RAW_HOOK_MANIFESTS[$index]}"
    grep -Fq '"command": "python3' "$source_manifest"
    if grep -Fq '"command": "agent-sec-python' "$source_manifest"; then
        echo "ERROR: shared adapter source uses the raw launcher: $source_manifest" >&2
        exit 1
    fi
    grep -Fq '"command": "agent-sec-python' "$raw_manifest"
    if grep -Fq '"command": "python3' "$raw_manifest"; then
        echo "ERROR: staged raw adapter still uses native Python: $raw_manifest" >&2
        exit 1
    fi
    if cmp -s "$source_manifest" "$raw_manifest"; then
        echo "ERROR: staged raw adapter was not adapted: $raw_manifest" >&2
        exit 1
    fi
done

if grep -Fq 'name = "python3"' "$STAGE/.anolisa/component.toml"; then
    echo "ERROR: staged raw contract still requires system Python" >&2
    exit 1
fi
if grep -Fq 'name = "python3"' "$ROOT/.anolisa/component.toml"; then
    echo "ERROR: component contract still requires system Python" >&2
    exit 1
fi
grep -Fq 'Requires:       python3 >= 3.11' "$ROOT/agent-sec-core.spec.in"
grep -Fq 'Requires:       python3 < 3.12' "$ROOT/agent-sec-core.spec.in"

FAKE_BIN="$TMP/fake-bin"
FAKE_PYTHON_LOG="$TMP/fake-python.log"
install -d -m 0755 "$FAKE_BIN"
cat > "$FAKE_BIN/python3" <<'EOF'
#!/bin/sh
printf 'called\n' >> "$ANOLISA_FAKE_PYTHON_LOG"
exit 99
EOF
chmod 0755 "$FAKE_BIN/python3"
for hook in \
    "$STAGE/adapters/sec-core/codex/hooks/hook.py" \
    "$STAGE/adapters/sec-core/qoder/hooks/hook.py" \
    "$STAGE/adapters/sec-core/qwencode/hooks/hook.py" \
    "$STAGE/adapters/sec-core/cosh/hooks/hook.py"; do
    ANOLISA_FAKE_PYTHON_LOG="$FAKE_PYTHON_LOG" \
    ANOLISA_TEST_PYTHON_LOG="$PYTHON_LOG" \
    PATH="$FAKE_BIN:$STAGE/bin:$PATH" \
        agent-sec-python "$hook"
    test "$(cat "$PYTHON_LOG")" = \
        "$STAGE/lib/anolisa/sec-core/python3.11/runtime/bin/python3.11"
done
test ! -e "$FAKE_PYTHON_LOG"

LIST="$TMP/tar-list.txt"
tar -tzf "$OUT_ONE/$ARTIFACT" > "$LIST"
for expected in \
    "./.anolisa/component.toml" \
    "./bin/agent-sec-cli" \
    "./bin/agent-sec-daemon" \
    "./bin/agent-sec-python" \
    "./bin/linux-sandbox" \
    "./lib/anolisa/sec-core/python3.11/runtime/bin/python3.11" \
    "./lib/anolisa/sec-core/python3.11/runtime/PROVENANCE.toml" \
    "./lib/anolisa/sec-core/python3.11/runtime/LICENSES.sha256" \
    "./lib/anolisa/sec-core/python3.11/runtime/lib/Tix8.4.3/license.terms" \
    "./lib/anolisa/sec-core/python3.11/runtime/lib/libpython3.11.so.1.0" \
    "./lib/anolisa/sec-core/python3.11/runtime/lib/python3.11/LICENSE.txt" \
    "./lib/anolisa/sec-core/python3.11/runtime/lib/tk8.6/demos/license.terms" \
    "./$ASSET_VERIFY_PACKAGE_PATH" \
    "./lib/anolisa/sec-core/python3.11/site-packages/agent_sec_cli/cli.py" \
    "./adapters/sec-core/openclaw/openclaw.plugin.json" \
    "./adapters/sec-core/hermes/plugin.yaml" \
    "./adapters/sec-core/codex/.codex-plugin/plugin.json" \
    "./adapters/sec-core/qoder/.qoder-plugin/plugin.json" \
    "./adapters/sec-core/qoder/hooks/hooks.json" \
    "./adapters/sec-core/qwencode/qwen-extension.json" \
    "./adapters/sec-core/cosh/cosh-extension.json" \
    "./share/anolisa/skills/code-scanner/SKILL.md" \
    "./share/anolisa/skills/prompt-scanner/SKILL.md" \
    "./share/anolisa/skills/pii-checker/SKILL.md" \
    "./share/anolisa/skills/security-observability/SKILL.md" \
    "./share/anolisa/skills/skill-ledger/SKILL.md" \
    "./share/anolisa/sec-core/agent-sec-core.service.in" \
    "./share/doc/sec-core/LICENSE"; do
    grep -Fxq "$expected" "$LIST" || {
        printf 'ERROR: missing archive entry %s\n' "$expected" >&2
        exit 1
    }
done

if grep -Eq '(^\./opt/|__pycache__|\.py[co]$|adapters/component\.toml)' "$LIST"; then
    echo "ERROR: raw package contains RPM paths, bytecode, or RPM contract" >&2
    exit 1
fi
if tar -tvzf "$OUT_ONE/$ARTIFACT" | grep -Eq '^[lh]'; then
    echo "ERROR: raw package contains symbolic or hard links" >&2
    exit 1
fi

tar -xzOf "$OUT_ONE/$ARTIFACT" ./.anolisa/component.toml > "$TMP/contract.toml"
cmp "$ROOT/.anolisa/component.toml" "$TMP/contract.toml"
python3 - "$TMP/contract.toml" "$STAGE" <<'PY'
import sys
import tomllib
from pathlib import Path

manifest = tomllib.loads(Path(sys.argv[1]).read_text())
stage = Path(sys.argv[2])
for framework in ("openclaw", "hermes"):
    adapter = next(item for item in manifest["adapters"] if item["framework"] == framework)
    skill = next(item for item in adapter[framework]["skills"] if item["name"] == "pii-checker")
    source = skill["source"].replace("{datadir}", str(stage / "share/anolisa"))
    assert (Path(source) / "SKILL.md").is_file(), (framework, source)
PY
tar -xzOf "$OUT_ONE/$ARTIFACT" "./$ASSET_VERIFY_PACKAGE_PATH" \
    > "$TMP/asset-verify-config.conf"
assert_asset_verify_config "$TMP/asset-verify-config.conf"
tar -xzOf "$OUT_ONE/$ARTIFACT" ./bin/agent-sec-cli > "$TMP/agent-sec-cli"
tar -xzOf "$OUT_ONE/$ARTIFACT" ./bin/agent-sec-python > "$TMP/agent-sec-python"
tar -xzOf "$OUT_ONE/$ARTIFACT" \
    ./share/anolisa/sec-core/agent-sec-core.service.in > "$TMP/service"
tar -xzOf "$OUT_ONE/$ARTIFACT" \
    ./adapters/sec-core/codex/hooks/hooks.json > "$TMP/codex-hooks.json"
tar -xzOf "$OUT_ONE/$ARTIFACT" \
    ./adapters/sec-core/qoder/hooks/hooks.json > "$TMP/qoder-hooks.json"
tar -xzOf "$OUT_ONE/$ARTIFACT" \
    ./adapters/sec-core/qwencode/qwen-extension.json > "$TMP/qwen-extension.json"
tar -xzOf "$OUT_ONE/$ARTIFACT" \
    ./adapters/sec-core/cosh/cosh-extension.json > "$TMP/cosh-extension.json"
for manifest in \
    "$TMP/codex-hooks.json" \
    "$TMP/qoder-hooks.json" \
    "$TMP/qwen-extension.json" \
    "$TMP/cosh-extension.json"; do
    grep -Fq '"command": "agent-sec-python' "$manifest"
    if grep -Fq '"command": "python3' "$manifest"; then
        echo "ERROR: packaged raw adapter still uses native Python: $manifest" >&2
        exit 1
    fi
done
grep -Fq 'agent-sec-python' "$TMP/agent-sec-cli"
grep -Fq 'lib/anolisa/sec-core/python3.11/runtime' "$TMP/agent-sec-python"
grep -Fq 'lib/anolisa/sec-core/python3.11/site-packages' "$TMP/agent-sec-python"
if grep -Fq 'PYTHONDONTWRITEBYTECODE' "$TMP/agent-sec-python"; then
    echo "ERROR: packaged Python wrapper disables bytecode persistence" >&2
    exit 1
fi
grep -Fq 'ExecStart="{bindir}/agent-sec-daemon" serve' "$TMP/service"
grep -Fq 'ReadWritePaths="{datadir}"' "$TMP/service"
grep -Fq 'render = "anolisa-paths-v1"' "$TMP/contract.toml"
grep -Fq 'min_anolisa_version = "0.2.17"' "$TMP/contract.toml"
grep -Fq 'framework_version = ">=2026.4.14"' "$TMP/contract.toml"
grep -Fq 'framework_version = ">=2026.4.24"' "$TMP/contract.toml"
grep -Fq 'name = "systemd"' "$TMP/contract.toml"
grep -Fq 'name = "nodejs"' "$TMP/contract.toml"
grep -Fq 'name = "jq"' "$TMP/contract.toml"
if grep -Fq 'name = "python3"' "$TMP/contract.toml"; then
    echo "ERROR: packaged raw contract still requires system Python" >&2
    exit 1
fi
test "$(grep -c 'min_anolisa_version' "$TMP/contract.toml")" = "1"
if grep -Eq '/opt/agent-sec|/usr/(local/)?share/anolisa|/usr/local/bin' \
    "$TMP/agent-sec-cli" "$TMP/service"; then
    echo "ERROR: raw wrapper or service template retained a fixed install path" >&2
    exit 1
fi

BAD_PYTHON_BUILD="$TMP/bad-python-build"
cp -a "$BUILD" "$BAD_PYTHON_BUILD"
printf 'corrupted license\n' >> \
    "$BAD_PYTHON_BUILD/python-runtime/lib/python3.11/LICENSE.txt"
if BUILD_DIR="$BAD_PYTHON_BUILD" \
    DESTDIR="$TMP/bad-python-stage" \
    TARGET_OS=linux \
    TARGET_ARCH=x86_64 \
        "$ROOT/packaging/raw/package.sh" stage \
        > "$TMP/bad-python.out" 2> "$TMP/bad-python.err"; then
    echo "ERROR: invalid Python license manifest unexpectedly succeeded" >&2
    exit 1
fi
grep -Fq "bundled Python license manifest is invalid" "$TMP/bad-python.err"

assert_raw_hook_bypass_rejected() {
    local name="$1"
    local hook_entry="$2"
    local bad_build="$TMP/bad-hook-$name-build"

    cp -a "$BUILD" "$bad_build"
    printf '{"hooks":[%s]}\n' "$hook_entry" > \
        "$bad_build/qwen-code-extension/unexpected-hooks.json"
    if BUILD_DIR="$bad_build" \
        DESTDIR="$TMP/bad-hook-$name-stage" \
        TARGET_OS=linux \
        TARGET_ARCH=x86_64 \
            "$ROOT/packaging/raw/package.sh" stage \
            > "$TMP/bad-hook-$name.out" 2> "$TMP/bad-hook-$name.err"; then
        echo "ERROR: raw hook bypass unexpectedly succeeded: $name" >&2
        exit 1
    fi
    grep -Fq "bypasses agent-sec-python" "$TMP/bad-hook-$name.err"
}

assert_raw_hook_bypass_rejected \
    compound \
    '{"command":"agent-sec-python hook.py && python3 -m unexpected_hook"}'
assert_raw_hook_bypass_rejected \
    env-command '{"command":"env python3 -m unexpected_hook"}'
assert_raw_hook_bypass_rejected \
    plain-python '{"command":"python -m unexpected_hook"}'
assert_raw_hook_bypass_rejected \
    env-args '{"command":"env","args":["python3","-m","unexpected_hook"]}'
assert_raw_hook_bypass_rejected \
    shell-args '{"command":"sh","args":["-c","python3 -m unexpected_hook"]}'

VERIFY_SOURCE="$TMP/verify-source"
VERSION_FILES=(
    "agent-sec-cli/pyproject.toml"
    "openclaw-plugin/openclaw.plugin.json"
    "openclaw-plugin/package.json"
    "hermes-plugin/src/plugin.yaml"
    "codex-plugin/hooks-plugin/.codex-plugin/plugin.json"
    "qoder-plugin/.qoder-plugin/plugin.json"
    "qwen-code-extension/qwen-extension.json"
    "cosh-extension/cosh-extension.json"
)
for relative in "${VERSION_FILES[@]}"; do
    install -D -m 0644 "$ROOT/$relative" "$VERIFY_SOURCE/$relative"
done
python3 "$ROOT/packaging/raw/verify_release.py" \
    "$VERIFY_SOURCE" "$ROOT/.anolisa/component.toml" > /dev/null

for relative in "${VERSION_FILES[@]}"; do
    BAD_SOURCE="$TMP/bad-source"
    BAD_VERSION="99.99.99"
    VERSION_PATTERN="${VERSION//./\\.}"
    rm -rf "$BAD_SOURCE"
    cp -a "$VERIFY_SOURCE" "$BAD_SOURCE"
    sed -i "0,/$VERSION_PATTERN/s//$BAD_VERSION/" "$BAD_SOURCE/$relative"
    if python3 "$ROOT/packaging/raw/verify_release.py" \
        "$BAD_SOURCE" "$ROOT/.anolisa/component.toml" \
        > "$TMP/bad.out" 2> "$TMP/bad.err"; then
        printf 'ERROR: version mismatch unexpectedly succeeded for %s\n' \
            "$relative" >&2
        exit 1
    fi
    grep -Fq "does not match $VERSION" "$TMP/bad.err"
done

BAD_CONTRACT="$TMP/bad-contract.toml"
sed '0,/name = "sec-core"/s//name = "wrong"/' \
    "$ROOT/.anolisa/component.toml" > "$BAD_CONTRACT"
if python3 "$ROOT/packaging/raw/verify_release.py" \
    "$VERIFY_SOURCE" "$BAD_CONTRACT" > "$TMP/bad.out" 2> "$TMP/bad.err"; then
    echo "ERROR: wrong component identity unexpectedly succeeded" >&2
    exit 1
fi
grep -Fq "expected 'sec-core'" "$TMP/bad.err"

sed '/name = "systemd"/d' \
    "$ROOT/.anolisa/component.toml" > "$BAD_CONTRACT"
if python3 "$ROOT/packaging/raw/verify_release.py" \
    "$VERIFY_SOURCE" "$BAD_CONTRACT" > "$TMP/bad.out" 2> "$TMP/bad.err"; then
    echo "ERROR: missing runtime dependency unexpectedly succeeded" >&2
    exit 1
fi
grep -Fq "missing runtime dependencies: systemd" "$TMP/bad.err"

cp "$ROOT/.anolisa/component.toml" "$BAD_CONTRACT"
cat >> "$BAD_CONTRACT" <<'EOF'

[[component.dependencies]]
name = "python3"
kind = "language-runtime"
version = ">=3.11,<3.12"
probe = "python3 --version"
source = "system"
EOF
if python3 "$ROOT/packaging/raw/verify_release.py" \
    "$VERIFY_SOURCE" "$BAD_CONTRACT" > "$TMP/bad.out" 2> "$TMP/bad.err"; then
    echo "ERROR: system Python dependency unexpectedly succeeded" >&2
    exit 1
fi
grep -Fq "must not declare the bundled Python as a dependency" "$TMP/bad.err"

sed '\|resource_root = "/opt/agent-sec/qoder-plugin/"|d' \
    "$ROOT/.anolisa/component.toml" > "$BAD_CONTRACT"
if python3 "$ROOT/packaging/raw/verify_release.py" \
    "$VERIFY_SOURCE" "$BAD_CONTRACT" > "$TMP/bad.out" 2> "$TMP/bad.err"; then
    echo "ERROR: missing RPM resource root unexpectedly succeeded" >&2
    exit 1
fi
grep -Fq "'qoder' RPM resource root is None" "$TMP/bad.err"

echo "OK: agent-sec-core raw package tests passed"
