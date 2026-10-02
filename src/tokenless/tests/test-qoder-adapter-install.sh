#!/usr/bin/env bash
# Regression tests for the native Qoder plugin lifecycle.
set -uo pipefail

PASS=0
FAIL=0

pass() { echo "[PASS] $1"; PASS=$((PASS + 1)); }
fail() { echo "[FAIL] $1" >&2; FAIL=$((FAIL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless"
SANDBOX="$(mktemp -d -t tokenless-qoder-install-test.XXXXXX)"
trap 'rm -rf "$SANDBOX"' EXIT

FAKE_HOME="$SANDBOX/home"
ADAPTER_DIR="$SANDBOX/adapter root"
QODERCLI="$FAKE_HOME/.qoder/bin/qodercli/qodercli"
mkdir -p "$FAKE_HOME/.qoder/bin/qodercli" "$ADAPTER_DIR"
cp -R "$SOURCE_ADAPTER_DIR"/. "$ADAPTER_DIR"/

# Source checkouts contain the version template; packages contain the stamped
# manifest. Stamp only the sandbox copy so this test never modifies the tree.
PLUGIN_TEMPLATE="$ADAPTER_DIR/qoder/.qoder-plugin/plugin.json.in" \
PLUGIN_JSON="$ADAPTER_DIR/qoder/.qoder-plugin/plugin.json" \
python3 - <<'PYEOF'
import os

with open(os.environ["PLUGIN_TEMPLATE"], encoding="utf-8") as source:
    content = source.read().replace("@VERSION@", "0.0.0-test")
with open(os.environ["PLUGIN_JSON"], "w", encoding="utf-8") as target:
    target.write(content)
PYEOF

cat > "$QODERCLI" <<'STUBEOF'
#!/usr/bin/env bash
set -euo pipefail

log="$HOME/.qoder/stub.log"
marker="$HOME/.qoder/native-tokenless-installed"
printf '%s\n' "$*" >> "$log"

if [ "${1:-}" = "plugins" ] && [ "${3:-}" = "--help" ]; then
    if [ "${2:-}" = "list" ] && [ "${QODER_STUB_MISSING_LIST:-0}" = "1" ]; then
        exit 1
    fi
    case "${2:-}" in
        install|list|uninstall|validate) exit 0 ;;
    esac
fi

if [ "${1:-}" = "plugins" ] && [ "${2:-}" = "validate" ]; then
    plugin_root="${3:?plugin root required}"
    PLUGIN_ROOT="$plugin_root" python3 - <<'PYEOF'
import json
import os
import pathlib

root = pathlib.Path(os.environ["PLUGIN_ROOT"])
with (root / ".qoder-plugin/plugin.json").open(encoding="utf-8") as source:
    manifest = json.load(source)
assert manifest["name"] == "tokenless"
assert not (root / "hooks.json").exists()
with (root / "hooks/hooks.json").open(encoding="utf-8") as source:
    hooks = json.load(source)["hooks"]
assert hooks["PreToolUse"] and hooks["PostToolUse"]
commands = list((root / "commands").iterdir())
assert commands and all(path.suffix == ".md" for path in commands)

adapter_root = root.parent
with (adapter_root / "common/tool-ready-spec.json").open(encoding="utf-8") as source:
    ready_spec = json.load(source)
assert {"run_in_terminal", "get_terminal_output"} <= set(ready_spec["Shell"]["aliases"])
assert {"grep_code", "search_file", "list_dir"} <= set(ready_spec["Read"]["aliases"])
assert {"create_file", "search_replace", "delete_file"} <= set(ready_spec["Write"]["aliases"])
assert {"search_web", "fetch_content"} <= set(ready_spec["WebFetch"]["aliases"])
PYEOF
    exit 0
fi

if [ "${1:-}" = "plugins" ] && [ "${2:-}" = "install" ]; then
    src="${3:?plugin path required}"
    [ "${4:-}" = "--scope" ] && [ "${5:-}" = "user" ]
    cache="$HOME/.qoder/plugins/cache/local/tokenless/0.0.0-test"
    rm -rf "$cache"
    mkdir -p "$cache"
    cp -R "$src"/. "$cache"/
    touch "$marker"
    exit 0
fi

if [ "${1:-}" = "plugins" ] && [ "${2:-}" = "list" ] && [ "${3:-}" = "--json" ]; then
    if [ ! -f "$marker" ]; then
        echo '[]'
    elif [ "${QODER_STUB_NO_HOOKS:-0}" = "1" ]; then
        echo '[{"id":"tokenless@local","scope":"user","enabled":true,"resources":{"hooks":[]}}]'
    else
        echo '[{"id":"tokenless@local","scope":"user","enabled":true,"resources":{"hooks":["hooks/hooks.json"]}}]'
    fi
    exit 0
fi

if [ "${1:-}" = "plugins" ] && [ "${2:-}" = "uninstall" ]; then
    [ "${3:-}" = "tokenless" ]
    [ "${4:-}" = "--scope" ] && [ "${5:-}" = "user" ]
    if [ ! -f "$marker" ]; then
        echo 'Plugin "tokenless" is not installed.' >&2
        exit 1
    fi
    if [ "${QODER_STUB_UNINSTALL_FAIL:-0}" = "1" ]; then
        echo 'simulated uninstall failure' >&2
        exit 1
    fi
    rm -f "$marker"
    exit 0
fi

echo "unsupported qodercli invocation: $*" >&2
exit 2
STUBEOF
chmod +x "$QODERCLI"

INSTALL_SH="$ADAPTER_DIR/qoder/scripts/install.sh"
UNINSTALL_SH="$ADAPTER_DIR/qoder/scripts/uninstall.sh"
DETECT_SH="$ADAPTER_DIR/qoder/scripts/detect.sh"
RUN_HOOK_SH="$ADAPTER_DIR/qoder/hooks/run-hook.sh"
SETTINGS="$FAKE_HOME/.qoder/settings.json"

cat > "$SETTINGS" <<'JSONEOF'
{
  "enabledPlugins": {"foreign@local": true},
  "hooks": {"PreToolUse": [{"hooks": [{"command": "keep-me"}]}]}
}
JSONEOF
settings_before="$(cksum "$SETTINGS")"

if HOME="$FAKE_HOME" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" bash "$DETECT_SH" >/dev/null; then
    pass "detect accepts a CLI with the required plugin lifecycle"
else
    fail "detect rejected a compatible qodercli"
fi

if HOME="$FAKE_HOME" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" bash "$INSTALL_SH" >/dev/null; then
    pass "native plugin installation succeeds"
else
    fail "native plugin installation failed"
fi

CACHE="$FAKE_HOME/.qoder/plugins/cache/local/tokenless/0.0.0-test"
if [ -f "$CACHE/hooks/hooks.json" ] && [ ! -f "$CACHE/hooks.json" ]; then
    pass "qodercli receives the native hooks/hooks.json layout"
else
    fail "cached plugin does not use the native hook layout"
fi

if grep -Fq '${QODER_PLUGIN_ROOT}/hooks/run-hook.sh' "$CACHE/hooks/hooks.json" && \
        ! grep -Fq 'QODER_TOKENLESS_HOOKS' "$CACHE/hooks/hooks.json"; then
    pass "hook commands preserve the Qoder runtime plugin-root placeholder"
else
    fail "hook commands do not use QODER_PLUGIN_ROOT correctly"
fi

if grep -Fqx "plugins install $ADAPTER_DIR/qoder --scope user" "$FAKE_HOME/.qoder/stub.log"; then
    pass "installer registers the original plugin root at user scope"
else
    fail "installer used an unexpected plugin path or scope"
fi

if [ "$(cksum "$SETTINGS")" = "$settings_before" ]; then
    pass "installer leaves settings.json untouched"
else
    fail "installer modified settings.json"
fi

if HOME="$FAKE_HOME" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" bash "$INSTALL_SH" >/dev/null; then
    pass "native plugin reinstallation is idempotent"
else
    fail "native plugin reinstallation failed"
fi

if HOME="$FAKE_HOME" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
        QODER_STUB_NO_HOOKS=1 bash "$INSTALL_SH" >/dev/null 2>&1; then
    fail "installer accepted an inventory entry with no loaded hooks"
else
    pass "installer rejects an installed plugin with no hook resources"
fi

if HOME="$FAKE_HOME" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
        QODER_STUB_MISSING_LIST=1 bash "$DETECT_SH" >/dev/null 2>&1; then
    fail "detect accepted a CLI without plugins list support"
else
    pass "detect rejects an incomplete plugin lifecycle"
fi

if HOME="$FAKE_HOME" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" bash "$UNINSTALL_SH" >/dev/null; then
    pass "native plugin uninstallation succeeds"
else
    fail "native plugin uninstallation failed"
fi

if [ ! -f "$FAKE_HOME/.qoder/native-tokenless-installed" ] && \
        grep -Fqx "plugins uninstall tokenless --scope user" "$FAKE_HOME/.qoder/stub.log"; then
    pass "uninstaller delegates user-scoped cleanup to qodercli"
else
    fail "uninstaller did not use the native qodercli lifecycle"
fi

uninstall_calls_before=$(grep -Fxc "plugins uninstall tokenless --scope user" \
    "$FAKE_HOME/.qoder/stub.log")
if HOME="$FAKE_HOME" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" bash "$UNINSTALL_SH" >/dev/null; then
    pass "repeated native plugin uninstallation succeeds"
else
    fail "repeated native plugin uninstallation was not idempotent"
fi
uninstall_calls_after=$(grep -Fxc "plugins uninstall tokenless --scope user" \
    "$FAKE_HOME/.qoder/stub.log")
if [ "$uninstall_calls_after" = "$uninstall_calls_before" ]; then
    pass "uninstaller skips qodercli mutation when the user plugin is absent"
else
    fail "uninstaller retried mutation for an absent plugin"
fi

if ! HOME="$FAKE_HOME" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" bash "$INSTALL_SH" >/dev/null; then
    fail "failed to reinstall plugin for uninstall failure coverage"
fi
if HOME="$FAKE_HOME" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
        QODER_STUB_UNINSTALL_FAIL=1 bash "$UNINSTALL_SH" >/dev/null 2>&1; then
    fail "uninstaller hid a real qodercli failure"
elif [ -f "$FAKE_HOME/.qoder/native-tokenless-installed" ]; then
    pass "uninstaller preserves and reports a real qodercli failure"
else
    fail "failed uninstall unexpectedly removed the plugin marker"
fi
if ! HOME="$FAKE_HOME" ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" bash "$UNINSTALL_SH" >/dev/null; then
    fail "failed to clean up plugin after uninstall failure coverage"
fi

if [ "$(cksum "$SETTINGS")" = "$settings_before" ]; then
    pass "uninstaller leaves settings.json untouched"
else
    fail "uninstaller modified settings.json"
fi

if run_hook_out="$(env -u HOME bash "$RUN_HOOK_SH" missing-hook.py 2>/dev/null)" && \
        [ "$run_hook_out" = "{}" ]; then
    pass "hook wrapper fails open when HOME is unset"
else
    fail "hook wrapper did not fail open without HOME: ${run_hook_out:-<no output>}"
fi

if traversal_out="$(bash "$RUN_HOOK_SH" ../../../etc/passwd 2>/dev/null)" && \
        [ "$traversal_out" = "{}" ]; then
    pass "hook wrapper rejects paths outside the hook allowlist"
else
    fail "hook wrapper accepted a traversal path: ${traversal_out:-<no output>}"
fi

echo ""
echo "Qoder adapter tests: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
