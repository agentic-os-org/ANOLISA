#!/usr/bin/env bash
# Regression tests for the Trae (TraeCode) adapter install lifecycle.
#
# Trae has no plugin system for hooks: install.sh merges the tokenless hook
# groups into the edition's global hooks.json (~/.trae-cn CN edition,
# ~/.trae international edition). The merge must be idempotent, must never
# clobber user-configured hooks, and uninstall.sh must remove exactly the
# tokenless-owned entries.
set -uo pipefail

PASS=0
FAIL=0

pass() { echo "[PASS] $1"; PASS=$((PASS + 1)); }
fail() { echo "[FAIL] $1" >&2; FAIL=$((FAIL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless"
SANDBOX="$(mktemp -d -t tokenless-trae-install-test.XXXXXX)"
trap 'rm -rf "$SANDBOX"' EXIT

ADAPTER_DIR="$SANDBOX/adapter root"
mkdir -p "$ADAPTER_DIR"
cp -R "$SOURCE_ADAPTER_DIR"/. "$ADAPTER_DIR"/

MARKER="TOKENLESS_AGENT_ID=trae"

count_tokenless_entries() {
    # Count hook command entries owned by tokenless inside a hooks.json.
    python3 - "$1" <<'PYEOF'
import json
import sys

marker = "TOKENLESS_AGENT_ID=trae"
with open(sys.argv[1], encoding="utf-8") as handle:
    config = json.load(handle)
count = 0
for groups in config.get("hooks", {}).values():
    if not isinstance(groups, list):
        continue
    for group in groups:
        for hook in group.get("hooks", []) if isinstance(group, dict) else []:
            command = hook.get("command") if isinstance(hook, dict) else None
            if isinstance(command, str) and marker in command:
                count += 1
print(count)
PYEOF
}

json_valid() { python3 -m json.tool "$1" >/dev/null 2>&1; }

# --- Test 1: source template is valid JSON and carries the marker ---------
if json_valid "$ADAPTER_DIR/trae/hooks/hooks.json" \
    && grep -qF "$MARKER" "$ADAPTER_DIR/trae/hooks/hooks.json" \
    && grep -qF "@TOKENLESS_ADAPTER_DIR@" "$ADAPTER_DIR/trae/hooks/hooks.json"; then
    pass "hooks template is valid JSON with marker and adapter-dir placeholder"
else
    fail "hooks template malformed"
fi

# --- Test 2: graceful no-op when no Trae edition home exists ---------------
export HOME="$SANDBOX/home-empty"
mkdir -p "$HOME"
if ANOLISA_TARGET=trae bash "$ADAPTER_DIR/trae/scripts/install.sh" >"$SANDBOX/out2" 2>&1; then
    if [ ! -e "$HOME/.trae-cn/hooks.json" ] && [ ! -e "$HOME/.trae/hooks.json" ] \
        && grep -qi "skipping" "$SANDBOX/out2"; then
        pass "install no-ops without Trae"
    else
        fail "install without Trae created config or missed skip message"
    fi
else
    fail "install without Trae exited non-zero"
fi

# --- Test 3: merge into existing CN-edition hooks.json, preserving user hooks
export HOME="$SANDBOX/home-cn"
mkdir -p "$HOME/.trae-cn"
cat > "$HOME/.trae-cn/hooks.json" <<'USERJSON'
{
  "version": 1,
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Write",
        "hooks": [
          { "type": "command", "command": "python3 /opt/user/lint.py", "timeout": 5 }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "echo user-stop-hook" }
        ]
      }
    ]
  }
}
USERJSON

if ANOLISA_TARGET=trae ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
    bash "$ADAPTER_DIR/trae/scripts/install.sh" >"$SANDBOX/out3" 2>&1; then
    pass "install into CN edition exits 0"
else
    fail "install into CN edition failed: $(cat "$SANDBOX/out3")"
fi

HOOKS_JSON="$HOME/.trae-cn/hooks.json"
python3 - "$HOOKS_JSON" "$ADAPTER_DIR" <<'PYEOF' && pass "merge preserves user hooks and stamps adapter dir" || fail "merge content wrong"
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    config = json.load(handle)
adapter_dir = sys.argv[2]
hooks = config["hooks"]

user_pre = [g for g in hooks["PreToolUse"]
            if any("lint.py" in (h.get("command") or "") for h in g.get("hooks", []))]
assert user_pre, "user PreToolUse hook lost"
assert hooks["Stop"], "user Stop hook lost"

commands = []
for groups in hooks.values():
    for group in groups:
        for hook in group.get("hooks", []):
            commands.append(hook.get("command", ""))
tokenless_cmds = [c for c in commands if "TOKENLESS_AGENT_ID=trae" in c]
assert len(tokenless_cmds) == 3, f"expected 3 tokenless hooks, got {len(tokenless_cmds)}"
assert all("@TOKENLESS_ADAPTER_DIR@" not in c for c in tokenless_cmds), "placeholder left unstamped"
assert all(f"{adapter_dir}/trae/hooks/run-hook.sh" in c for c in tokenless_cmds), "adapter dir not stamped"
assert any("rewrite_hook.py" in c and '"matcher"' not in c for c in tokenless_cmds)
assert any(c.endswith("rewrite_hook.py") for c in tokenless_cmds)
assert any(c.endswith("tool_ready_hook.sh") for c in tokenless_cmds)
assert any(c.endswith("compress_response_hook.py") for c in tokenless_cmds)

matchers = {g.get("matcher"): g for g in hooks["PreToolUse"]}
assert "RunCommand" in matchers, "rewrite hook must match RunCommand"
PYEOF

# --- Test 4: install is idempotent -----------------------------------------
if ANOLISA_TARGET=trae ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
    bash "$ADAPTER_DIR/trae/scripts/install.sh" >/dev/null 2>&1; then
    entries="$(count_tokenless_entries "$HOOKS_JSON")"
    if [ "$entries" = "3" ]; then
        pass "install is idempotent (3 tokenless entries after reinstall)"
    else
        fail "reinstall changed tokenless entry count: $entries"
    fi
else
    fail "reinstall exited non-zero"
fi

# --- Test 5: both editions get hooks when both homes exist ------------------
export HOME="$SANDBOX/home-both"
mkdir -p "$HOME/.trae-cn" "$HOME/.trae"
if ANOLISA_TARGET=trae ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
    bash "$ADAPTER_DIR/trae/scripts/install.sh" >/dev/null 2>&1 \
    && [ "$(count_tokenless_entries "$HOME/.trae-cn/hooks.json")" = "3" ] \
    && [ "$(count_tokenless_entries "$HOME/.trae/hooks.json")" = "3" ]; then
    pass "install covers both CN and international editions"
else
    fail "install did not cover both editions"
fi

# --- Test 6: uninstall removes only tokenless entries -----------------------
export HOME="$SANDBOX/home-cn"
if ANOLISA_TARGET=trae bash "$ADAPTER_DIR/trae/scripts/uninstall.sh" >"$SANDBOX/out6" 2>&1; then
    entries="$(count_tokenless_entries "$HOOKS_JSON")"
    if [ "$entries" = "0" ] && json_valid "$HOOKS_JSON" \
        && grep -qF "lint.py" "$HOOKS_JSON" && grep -qF "user-stop-hook" "$HOOKS_JSON"; then
        pass "uninstall removes tokenless entries, keeps user hooks"
    else
        fail "uninstall left wrong state (entries=$entries)"
    fi
else
    fail "uninstall exited non-zero: $(cat "$SANDBOX/out6")"
fi

# --- Test 7: uninstall is idempotent ----------------------------------------
if ANOLISA_TARGET=trae bash "$ADAPTER_DIR/trae/scripts/uninstall.sh" >/dev/null 2>&1; then
    pass "uninstall idempotent"
else
    fail "second uninstall exited non-zero"
fi

# --- Test 8: invalid existing hooks.json is never clobbered ------------------
export HOME="$SANDBOX/home-bad"
mkdir -p "$HOME/.trae-cn"
echo '{ not json' > "$HOME/.trae-cn/hooks.json"
if ANOLISA_TARGET=trae ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
    bash "$ADAPTER_DIR/trae/scripts/install.sh" >/dev/null 2>&1; then
    fail "install accepted an invalid hooks.json"
else
    if grep -qF '{ not json' "$HOME/.trae-cn/hooks.json"; then
        pass "install refuses invalid hooks.json without clobbering it"
    else
        fail "invalid hooks.json was modified"
    fi
fi

# --- Test 9: detect.sh tri-state --------------------------------------------
export HOME="$SANDBOX/home-detect"
mkdir -p "$HOME"
ANOLISA_TARGET=trae ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
    bash "$ADAPTER_DIR/trae/scripts/detect.sh" >/dev/null 2>&1
detect_rc=$?
if [ "$detect_rc" -eq 2 ]; then
    pass "detect.sh exits 2 (missing prereqs) without Trae"
else
    fail "detect.sh unexpected exit code: $detect_rc"
fi

echo ""
echo "Results: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
