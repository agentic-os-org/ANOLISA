#!/usr/bin/env bash
# Fixture-driven regression tests for the V2 raw package.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

BUILD="$TMP/build"
VERSION="$(python3 "$ROOT/packaging/raw/verify_release.py" \
    "$ROOT" "$ROOT/packaging/anolisa/component.toml")"

install -d -m 0755 \
    "$BUILD/v2/bin" \
    "$BUILD/python-runtime/bin" \
    "$BUILD/python-runtime/lib/Tix8.4.3" \
    "$BUILD/python-runtime/lib/python3.11" \
    "$BUILD/python-runtime/lib/tk8.6/demos" \
    "$BUILD/openclaw-plugin" \
    "$BUILD/hermes-plugin/src" \
    "$BUILD/codex-plugin/hooks-plugin/.codex-plugin" \
    "$BUILD/codex-plugin/hooks-plugin/hooks" \
    "$BUILD/qoder-plugin/.qoder-plugin" \
    "$BUILD/qoder-plugin/hooks" \
    "$BUILD/qwen-code-extension/hooks" \
    "$BUILD/cosh-extension/hooks"

for binary in agent-sec-cli agent-sec-daemon agent-sec-daemon-health; do
    printf '#!/bin/sh\nexit 0\n' > "$BUILD/v2/bin/$binary"
    chmod 0755 "$BUILD/v2/bin/$binary"
done
printf '#!/bin/sh\nexit 0\n' > "$BUILD/linux-sandbox"
chmod 0755 "$BUILD/linux-sandbox"
printf '#!/bin/sh\nexit 0\n' > "$BUILD/python-runtime/bin/python3.11"
chmod 0755 "$BUILD/python-runtime/bin/python3.11"
printf 'license\n' > "$BUILD/python-runtime/lib/python3.11/LICENSE.txt"
printf 'license\n' > "$BUILD/python-runtime/lib/Tix8.4.3/license.terms"
printf 'license\n' > "$BUILD/python-runtime/lib/tk8.6/demos/license.terms"
cp "$ROOT/packaging/raw/assets/python-runtime/PROVENANCE.toml" \
    "$BUILD/python-runtime/PROVENANCE.toml"
(
    cd "$BUILD/python-runtime"
    sha256sum \
        lib/Tix8.4.3/license.terms \
        lib/python3.11/LICENSE.txt \
        lib/tk8.6/demos/license.terms > LICENSES.sha256
)

cp "$ROOT/openclaw-plugin/openclaw.plugin.json" "$BUILD/openclaw-plugin/"
cp "$ROOT/hermes-plugin/src/plugin.yaml" "$BUILD/hermes-plugin/src/"
cp "$ROOT/codex-plugin/hooks-plugin/.codex-plugin/plugin.json" \
    "$BUILD/codex-plugin/hooks-plugin/.codex-plugin/"
cp "$ROOT/codex-plugin/hooks-plugin/hooks/hooks.json" \
    "$BUILD/codex-plugin/hooks-plugin/hooks/"
cp "$ROOT/qoder-plugin/.qoder-plugin/plugin.json" "$BUILD/qoder-plugin/.qoder-plugin/"
cp "$ROOT/qoder-plugin/hooks/hooks.json" "$BUILD/qoder-plugin/hooks/"
cp "$ROOT/qwen-code-extension/qwen-extension.json" "$BUILD/qwen-code-extension/"
cp "$ROOT/cosh-extension/cosh-extension.json" "$BUILD/cosh-extension/"
make -C "$ROOT" stage-skills stage-gateway BUILD_DIR="$BUILD"

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

for binary in agent-sec-cli agent-sec-daemon agent-sec-daemon-health agent-sec-python linux-sandbox; do
    test "$(stat -c '%a' "$STAGE/bin/$binary")" = "755"
done
test ! -e "$STAGE/lib/anolisa/sec-core/python3.11/site-packages"
cmp "$ROOT/packaging/anolisa/component.toml" "$STAGE/.anolisa/component.toml"
grep -Fq 'WantedBy=multi-user.target' \
    "$STAGE/share/anolisa/sec-core/agent-sec-core.service.in"

tar -tzf "$OUT_ONE/$ARTIFACT" > "$TMP/tar-list.txt"
for expected in \
    "./bin/agent-sec-cli" \
    "./bin/agent-sec-daemon" \
    "./bin/agent-sec-daemon-health" \
    "./bin/agent-sec-python" \
    "./lib/anolisa/sec-core/python3.11/runtime/bin/python3.11"; do
    grep -Fxq "$expected" "$TMP/tar-list.txt"
done
if grep -Eq 'site-packages/agent_sec_cli|agent-sec-cli-wrapper|agent-sec-daemon-wrapper' \
    "$TMP/tar-list.txt"; then
    echo "ERROR: V2 raw archive contains V1 CLI/daemon payload" >&2
    exit 1
fi

python3 - "$ROOT" "$STAGE" "$TMP" <<'PY'
import os
import subprocess
import sys
import tomllib
from pathlib import Path

root, stage, temporary = map(Path, sys.argv[1:])
gateway = stage / "share/anolisa/sec-core/gateway"
for name, source in {
    "asc_gateway_addon.py": "v2/gateway/addon/asc_gateway_addon.py",
    "asc_gateway_policy.py": "v2/gateway/addon/asc_gateway_policy.py",
    "github-policy.json": "v2/crates/asc-gateway-plan/policies/github.json",
    "mitmproxy.provenance": "v2/gateway/mitmproxy.provenance",
    "install-mitmproxy.sh": "packaging/gateway/install-mitmproxy.sh",
}.items():
    assert (gateway / name).read_bytes() == (root / source).read_bytes(), name
    assert (gateway / name).stat().st_mode & 0o777 == (0o755 if name.endswith(".sh") else 0o644)
unit = stage / "share/anolisa/sec-core/agent-sec-gateway.service.in"
assert '-s "{datadir}/sec-core/gateway/asc_gateway_addon.py"' in unit.read_text()
assert "[Install]" not in unit.read_text()
for path in ("opt/agent-sec-gateway", "run/agent-sec-gateway", "var/lib/agent-sec/credentials"):
    assert not (stage / path).exists(), path
manifest = stage / ".anolisa/component.toml"
component = tomllib.loads(manifest.read_text())["component"]
assert all(not service["enable"] and not service["start"] for service in component["services"])

hook_bin = temporary / "hook-bin"
hook_bin.mkdir()
mock_cli = hook_bin / "agent-sec-cli"
mock_cli.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$RAW_HOOK_CALLS"\nexit "$RAW_DISABLE_EXIT"\n')
mock_manager = hook_bin / "systemctl"
mock_manager.write_text('#!/bin/sh\n[ "$*" = "is-active --quiet agent-sec-gateway.service" ] || exit 2\nexit "$RAW_UNIT_EXIT"\n')
for executable in (mock_cli, mock_manager):
    executable.chmod(0o755)
runtime = temporary / "active.json"
script = temporary / "pre-uninstall-gateway.sh"
script.write_text(
    (stage / "share/anolisa/sec-core/pre-uninstall-gateway.sh.in").read_text()
    .replace("{bindir}", str(hook_bin))
    .replace("/run/agent-sec-gateway/active.json", str(runtime))
)
script.chmod(0o755)
for unit_exit, disable_exit, manifest_present, expected in (
    (1, 1, False, 0),
    (0, 0, False, 0),
    (0, 1, False, 1),
    (1, 0, True, 0),
):
    if manifest_present:
        runtime.write_text("{}")
    elif runtime.exists():
        runtime.unlink()
    calls = temporary / "withdrawal-calls"
    if calls.exists():
        calls.unlink()
    result = subprocess.run(
        [str(script)], check=False, capture_output=True, text=True,
        env={**os.environ, "PATH": f"{hook_bin}:{os.environ['PATH']}",
             "RAW_UNIT_EXIT": str(unit_exit), "RAW_DISABLE_EXIT": str(disable_exit),
             "RAW_HOOK_CALLS": str(calls)},
    )
    assert result.returncode == expected, result.stderr
    assert (calls.read_text() if calls.exists() else "") == (
        "gateway disable\n" if unit_exit == 0 or manifest_present else ""
    )

verifier = [sys.executable, str(root / "packaging/raw/verify_release.py"),
            str(root), str(root / "packaging/anolisa/component.toml"),
            "--payload-root", str(stage)]
asset = gateway / "asc_gateway_addon.py"
saved = asset.with_suffix(".saved")
asset.rename(saved)
try:
    result = subprocess.run(verifier, check=False, capture_output=True, text=True)
    assert result.returncode != 0 and "raw gateway asset" in result.stderr
finally:
    saved.rename(asset)
original = manifest.read_text()
try:
    manifest.write_text(original + '\n[[component.services]]\nunit = "agent-sec-gateway.service"\nscope = "system"\nenable = false\nstart = false\n')
    result = subprocess.run(verifier, check=False, capture_output=True, text=True)
    assert result.returncode != 0 and "must remain daemon-owned" in result.stderr
finally:
    manifest.write_text(original)
PY

SCORE_PAYLOAD="$TMP/score-payload.tar.gz"
python3 - "$SCORE_PAYLOAD" "$ROOT" <<'PY'
import io
import sys
import tarfile
from pathlib import Path

with tarfile.open(sys.argv[1], "w:gz") as archive:
    for name in (
        "skill-score-cli/bin/python3.11",
        "skill-score-cli/bin/agent-sec-cli",
        "skill-score-runtime/bin/python3.12",
    ):
        data = b"#!/bin/sh\nexit 0\n"
        if name.endswith("agent-sec-cli"):
            data = (Path(sys.argv[2]) / "packaging/skill-score/agent-sec-cli").read_bytes()
        member = tarfile.TarInfo("opt/agent-sec/" + name)
        member.size, member.mode = len(data), 0o755
        archive.addfile(member, io.BytesIO(data))
PY
PREVIEW_VERSION="${VERSION}-skillscore.123456789abc"
if RAW_SKILL_SCORE_PAYLOAD="$SCORE_PAYLOAD" RAW_PREVIEW_VERSION="$PREVIEW_VERSION" \
    run_package "$TMP/legacy-score-out" > "$TMP/legacy-score.log" 2>&1; then
    echo "ERROR: base raw package accepted an embedded scoring runtime" >&2
    exit 1
fi
grep -Fq 'base packages cannot embed assessment payloads' "$TMP/legacy-score.log"
test ! -e "$TMP/legacy-score-out/sec-core-${PREVIEW_VERSION}-linux-x86_64.tar.gz"
RAW_PREVIEW_VERSION="$PREVIEW_VERSION" run_package "$TMP/score-out"
mkdir "$TMP/score-stage"
tar -xzf "$TMP/score-out/sec-core-${PREVIEW_VERSION}-linux-x86_64.tar.gz" \
    -C "$TMP/score-stage"
cmp "$BUILD/v2/bin/agent-sec-cli" "$TMP/score-stage/bin/agent-sec-cli"
test ! -e "$TMP/score-stage/bin/agent-sec-skill-score"
test ! -e "$TMP/score-stage/lib/anolisa/sec-core/skill-score"
test ! -e "$TMP/score-stage/lib/anolisa/sec-core-skill-score"
grep -Fxq "version = \"$PREVIEW_VERSION\"" "$TMP/score-stage/.anolisa/component.toml"
test -z "$(find "$TMP/score-stage" -name '*.tar*' -print -quit)"
VARIANT_VERSION="$PREVIEW_VERSION.base.abcdef012345"
RAW_PREVIEW_VERSION="$VARIANT_VERSION" run_package "$TMP/variant-out"
tar -xOzf "$TMP/variant-out/sec-core-${VARIANT_VERSION}-linux-x86_64.tar.gz" \
    ./.anolisa/component.toml | grep -Fxq "version = \"$VARIANT_VERSION\""
if RAW_PREVIEW_VERSION="$PREVIEW_VERSION.base.invalid" run_package "$TMP/invalid-variant" \
    > "$TMP/invalid-variant.log" 2>&1; then
    echo "ERROR: base raw package accepted an invalid image variant" >&2
    exit 1
fi
python3 "$ROOT/tests/packaging/test_skill_score_raw.py"

echo "OK: agent-sec-core V2 raw package tests passed"
