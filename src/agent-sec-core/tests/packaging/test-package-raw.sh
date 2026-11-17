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
cp "$ROOT/cosh-extension/hooks/"*.py "$BUILD/cosh-extension/hooks/"
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
    "./share/anolisa/sec-core/sandbox/permissions.json.template" \
    "./lib/anolisa/sec-core/python3.11/runtime/bin/python3.11"; do
    grep -Fxq "$expected" "$TMP/tar-list.txt"
done
if grep -Eq 'site-packages/agent_sec_cli|agent-sec-cli-wrapper|agent-sec-daemon-wrapper' \
    "$TMP/tar-list.txt"; then
    echo "ERROR: V2 raw archive contains V1 CLI/daemon payload" >&2
    exit 1
fi

python3 - "$ROOT" "$STAGE" "$TMP" <<'PY'
import json
import os
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

root, stage, temporary = map(Path, sys.argv[1:])
template = stage / "share/anolisa/sec-core/sandbox/permissions.json.template"
assert template.read_bytes() == (root / "packaging/sandbox/permissions.json.template").read_bytes()
contract = tomllib.loads((stage / ".anolisa/component.toml").read_text())
assert any(entry.get("target") == "{etcdir}/sec-core/sandbox/permissions.json.template"
           and entry.get("type") == "config" for entry in contract["component"]["layout"]["files"])
paths_source = stage / "share/anolisa/sec-core/sandbox_paths.py.in"
assert not (stage / "adapters/sec-core/cosh/hooks/sandbox_paths.py").exists()
assert '{etcdir}/sec-core/sandbox/permissions.json.template' in paths_source.read_text()
assert any(entry.get("source") == "share/anolisa/sec-core/sandbox_paths.py.in"
           and entry.get("render") == "anolisa-paths-v1" for entry in contract["component"]["layout"]["files"])

# Materialize the declared destinations, then start the installed guard in both modes.
layout = {"datadir": str(temporary / "installed/share/anolisa"),
          "etcdir": str(temporary / "installed/etc/anolisa")}
adapter = next(item for item in contract["adapters"] if item["framework"] == "cosh")
installed = Path(adapter["dest"].format_map(layout))
shutil.copytree(stage / adapter["source"], installed)
for entry in contract["component"]["layout"]["files"]:
    if entry["source"] not in ("share/anolisa/sec-core/sandbox_paths.py.in",
                               "share/anolisa/sec-core/sandbox/permissions.json.template"):
        continue
    destination = Path(entry["target"].format_map(layout))
    destination.parent.mkdir(parents=True, exist_ok=True)
    content = (stage / entry["source"]).read_text()
    if entry.get("render") == "anolisa-paths-v1":
        for key, value in layout.items():
            content = content.replace("{" + key + "}", value)
    destination.write_text(content)
cli_bin = temporary / "sandbox-cli"
cli_bin.mkdir()
cli = cli_bin / "agent-sec-cli"
calls = temporary / "sandbox-calls.json"
cli.write_text(
    f"#!{sys.executable}\nimport json, sys\nfrom pathlib import Path\n"
    f"Path({str(calls)!r}).write_text(json.dumps([sys.argv[1:], json.load(sys.stdin)]))\n"
    "print(json.dumps({'decision': 'block', 'reason': 'fixture policy denied', "
    "'systemMessage': 'fixture policy denied'}))\n"
)
cli.chmod(0o755)
environment = os.environ | {"PATH": str(cli_bin) + os.pathsep + os.environ["PATH"],
                            "PYTHONDONTWRITEBYTECODE": "1"}
environment.pop("PYTHONPATH", None)
environment.pop("AGENT_SEC_COSH_SANDBOX_PERMISSION_SOURCE", None)
environment.pop("AGENT_SEC_COSH_SANDBOX_RUNTIME_PERMISSIONS", None)
payload = {"hook_event_name": "PreToolUse", "tool_name": "shell",
           "tool_input": {"command": "python3 skill.py"}, "cwd": str(temporary),
           "session_id": "raw-install-fixture"}
for enabled, decision in (("0", "allow"), ("1", "block")):
    # Direct protocol fixture: simulate Cosh's per-invocation output policy.
    output_policy = "block" if enabled == "1" else "allow"
    result = subprocess.run(
        [sys.executable, "-B", str(installed / "hooks/sandbox-guard.py")],
        input=json.dumps(payload), text=True, capture_output=True, check=False,
        timeout=10, cwd=temporary,
        env=environment | {"AGENT_SEC_COSH_SANDBOX_ALL_SHELL": enabled,
                           "COSH_HOOK_EMPTY_OUTPUT_POLICY": output_policy},
    )
    assert result.returncode == 0, result.stderr
    output = json.loads(result.stdout)
    assert output["decision"] == decision, output
    if enabled == "0":
        assert not calls.exists()
    else:
        arguments, request = json.loads(calls.read_text())
        assert arguments == ["sandbox", "hook", "prepare", "--permissions",
                             layout["etcdir"] + "/sec-core/sandbox/permissions.json.template",
                             "--timeout-ms", "5000"]
        assert request["tool_input"] == payload["tool_input"]
        assert output["reason"] == "fixture policy denied"
gateway = stage / "share/anolisa/sec-core/gateway"
for name, source in {
    "asc_gateway_addon.py": "v2/gateway/addon/asc_gateway_addon.py",
    "asc_gateway_policy.py": "v2/gateway/addon/asc_gateway_policy.py",
    "github.cedar": "v2/crates/asc-gateway-plan/policies/github.cedar",
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

echo "OK: agent-sec-core V2 raw package tests passed"
