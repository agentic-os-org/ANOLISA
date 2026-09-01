#!/usr/bin/env bash
# Regression tests for the WorkBuddy (Tencent CodeBuddy) adapter install
# lifecycle.
#
# WorkBuddy desktop, WorkBuddy Enterprise and the CodeBuddy CLI share the
# ~/.codebuddy/settings.json hook protocol, so install.sh merges the
# tokenless hook groups into the user-level settings.json, whose "hooks"
# key follows the Claude Code matcher-group shape. The merge must be
# idempotent, must preserve user-configured hooks AND every other settings
# key, must never loosen the settings.json file mode, and uninstall.sh
# must remove exactly the tokenless-owned entries.
set -uo pipefail

PASS=0
FAIL=0

pass() { echo "[PASS] $1"; PASS=$((PASS + 1)); }
fail() { echo "[FAIL] $1" >&2; FAIL=$((FAIL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ADAPTER_DIR="$SCRIPT_DIR/../adapters/tokenless"
SANDBOX="$(mktemp -d -t tokenless-workbuddy-install-test.XXXXXX)"
trap 'rm -rf "$SANDBOX"' EXIT

ADAPTER_DIR="$SANDBOX/adapter root"
mkdir -p "$ADAPTER_DIR"
cp -R "$SOURCE_ADAPTER_DIR"/. "$ADAPTER_DIR"/

MARKER="TOKENLESS_AGENT_ID=workbuddy"

count_tokenless_entries() {
    python3 - "$1" <<'PYEOF'
import json
import sys

marker = "TOKENLESS_AGENT_ID=workbuddy"
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
if json_valid "$ADAPTER_DIR/workbuddy/hooks/hooks.json" \
    && grep -qF "$MARKER" "$ADAPTER_DIR/workbuddy/hooks/hooks.json" \
    && grep -qF "@TOKENLESS_ADAPTER_DIR@" "$ADAPTER_DIR/workbuddy/hooks/hooks.json"; then
    pass "hooks template is valid JSON with marker and adapter-dir placeholder"
else
    fail "hooks template malformed"
fi

# --- Test 2: graceful no-op when the .codebuddy home does not exist --------
export HOME="$SANDBOX/home-empty"
mkdir -p "$HOME"
if ANOLISA_TARGET=workbuddy bash "$ADAPTER_DIR/workbuddy/scripts/install.sh" >"$SANDBOX/out2" 2>&1; then
    if [ ! -e "$HOME/.codebuddy/settings.json" ] && grep -qi "skipping" "$SANDBOX/out2"; then
        pass "install no-ops without WorkBuddy"
    else
        fail "install without WorkBuddy created config or missed skip message"
    fi
else
    fail "install without WorkBuddy exited non-zero"
fi

# --- Test 3: merge into existing settings.json, preserving user hooks and
#             unrelated settings keys
export HOME="$SANDBOX/home-wb"
mkdir -p "$HOME/.codebuddy"
cat > "$HOME/.codebuddy/settings.json" <<'USERJSON'
{
  "model": "default-model",
  "permissions": { "allow": ["Bash(git *)"] },
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

if ANOLISA_TARGET=workbuddy ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
    bash "$ADAPTER_DIR/workbuddy/scripts/install.sh" >"$SANDBOX/out3" 2>&1; then
    pass "install exits 0"
else
    fail "install failed: $(cat "$SANDBOX/out3")"
fi

SETTINGS_JSON="$HOME/.codebuddy/settings.json"
python3 - "$SETTINGS_JSON" "$ADAPTER_DIR" <<'PYEOF' && pass "merge preserves user settings/hooks and stamps adapter dir" || fail "merge content wrong"
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    config = json.load(handle)
adapter_dir = sys.argv[2]

# Unrelated settings keys survive untouched.
assert config["model"] == "default-model", "model key lost"
assert config["permissions"] == {"allow": ["Bash(git *)"]}, "permissions key lost"

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
tokenless_cmds = [c for c in commands if "TOKENLESS_AGENT_ID=workbuddy" in c]
assert len(tokenless_cmds) == 3, f"expected 3 tokenless hooks, got {len(tokenless_cmds)}"
assert all("@TOKENLESS_ADAPTER_DIR@" not in c for c in tokenless_cmds), "placeholder left unstamped"
assert all(f"{adapter_dir}/workbuddy/hooks/run-hook.sh" in c for c in tokenless_cmds), "adapter dir not stamped"
assert any(c.endswith("rewrite_hook.py") for c in tokenless_cmds)
assert any(c.endswith("tool_ready_hook.sh") for c in tokenless_cmds)
assert any(c.endswith("compress_response_hook.py") for c in tokenless_cmds)

matchers = {g.get("matcher"): g for g in hooks["PreToolUse"]}
assert "Bash" in matchers, "rewrite hook must match Bash"
PYEOF

# --- Test 4: install is idempotent -----------------------------------------
if ANOLISA_TARGET=workbuddy ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
    bash "$ADAPTER_DIR/workbuddy/scripts/install.sh" >/dev/null 2>&1; then
    entries="$(count_tokenless_entries "$SETTINGS_JSON")"
    if [ "$entries" = "3" ]; then
        pass "install is idempotent (3 tokenless entries after reinstall)"
    else
        fail "reinstall changed tokenless entry count: $entries"
    fi
else
    fail "reinstall exited non-zero"
fi

# --- Test 5: settings.json is created when only the home dir exists --------
export HOME="$SANDBOX/home-fresh"
mkdir -p "$HOME/.codebuddy"
if ANOLISA_TARGET=workbuddy ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
    bash "$ADAPTER_DIR/workbuddy/scripts/install.sh" >/dev/null 2>&1 \
    && [ "$(count_tokenless_entries "$HOME/.codebuddy/settings.json")" = "3" ]; then
    pass "install creates settings.json in an existing .codebuddy home"
else
    fail "install did not create settings.json"
fi

# --- Test 5a: install/uninstall never widen settings.json permissions -------
# CodeBuddy allows credentials in the settings env field, so a 0600 file must
# stay 0600 across both scripts, even under a permissive umask. The
# replacement inode must also keep the existing owner/group: mkstemp creates
# it with the installer's UID/GID, so the scripts restore ownership before
# os.replace (a root- or cross-account run must not reassign the file).
export HOME="$SANDBOX/home-perms"
mkdir -p "$HOME/.codebuddy"
echo '{"model": "default-model"}' > "$HOME/.codebuddy/settings.json"
chmod 0600 "$HOME/.codebuddy/settings.json"
owner_before="$(stat -c '%u:%g' "$HOME/.codebuddy/settings.json" 2>/dev/null || stat -f '%u:%g' "$HOME/.codebuddy/settings.json")"
(
    umask 022
    ANOLISA_TARGET=workbuddy ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
        bash "$ADAPTER_DIR/workbuddy/scripts/install.sh" >/dev/null 2>&1
)
mode="$(stat -c '%a' "$HOME/.codebuddy/settings.json" 2>/dev/null || stat -f '%Lp' "$HOME/.codebuddy/settings.json")"
owner_after="$(stat -c '%u:%g' "$HOME/.codebuddy/settings.json" 2>/dev/null || stat -f '%u:%g' "$HOME/.codebuddy/settings.json")"
if [ "$mode" = "600" ]; then
    pass "install preserves existing 0600 settings.json mode under umask 022"
else
    fail "install widened settings.json mode to $mode"
fi
if [ "$owner_after" = "$owner_before" ]; then
    pass "install preserves settings.json owner/group"
else
    fail "install changed settings.json owner/group from $owner_before to $owner_after"
fi
(
    umask 022
    ANOLISA_TARGET=workbuddy bash "$ADAPTER_DIR/workbuddy/scripts/uninstall.sh" >/dev/null 2>&1
)
mode="$(stat -c '%a' "$HOME/.codebuddy/settings.json" 2>/dev/null || stat -f '%Lp' "$HOME/.codebuddy/settings.json")"
owner_after="$(stat -c '%u:%g' "$HOME/.codebuddy/settings.json" 2>/dev/null || stat -f '%u:%g' "$HOME/.codebuddy/settings.json")"
if [ "$mode" = "600" ]; then
    pass "uninstall preserves existing 0600 settings.json mode under umask 022"
else
    fail "uninstall widened settings.json mode to $mode"
fi
if [ "$owner_after" = "$owner_before" ]; then
    pass "uninstall preserves settings.json owner/group"
else
    fail "uninstall changed settings.json owner/group from $owner_before to $owner_after"
fi

# --- Test 5b: a permissive existing mode is left untouched -------------------
chmod 0644 "$HOME/.codebuddy/settings.json"
(
    umask 022
    ANOLISA_TARGET=workbuddy ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
        bash "$ADAPTER_DIR/workbuddy/scripts/install.sh" >/dev/null 2>&1
)
mode="$(stat -c '%a' "$HOME/.codebuddy/settings.json" 2>/dev/null || stat -f '%Lp' "$HOME/.codebuddy/settings.json")"
if [ "$mode" = "644" ]; then
    pass "install keeps an existing 0644 settings.json mode unchanged"
else
    fail "install changed existing 0644 mode to $mode"
fi

# --- Test 5c: freshly created settings.json starts restrictive ---------------
export HOME="$SANDBOX/home-perms-fresh"
mkdir -p "$HOME/.codebuddy"
(
    umask 022
    ANOLISA_TARGET=workbuddy ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
        bash "$ADAPTER_DIR/workbuddy/scripts/install.sh" >/dev/null 2>&1
)
mode="$(stat -c '%a' "$HOME/.codebuddy/settings.json" 2>/dev/null || stat -f '%Lp' "$HOME/.codebuddy/settings.json")"
if [ "$mode" = "600" ]; then
    pass "install creates settings.json with 0600"
else
    fail "install created settings.json with mode $mode"
fi

# --- Test 6: uninstall removes only tokenless entries -----------------------
export HOME="$SANDBOX/home-wb"
if ANOLISA_TARGET=workbuddy bash "$ADAPTER_DIR/workbuddy/scripts/uninstall.sh" >"$SANDBOX/out6" 2>&1; then
    entries="$(count_tokenless_entries "$SETTINGS_JSON")"
    if [ "$entries" = "0" ] && json_valid "$SETTINGS_JSON" \
        && grep -qF "lint.py" "$SETTINGS_JSON" && grep -qF "user-stop-hook" "$SETTINGS_JSON" \
        && grep -qF '"model"' "$SETTINGS_JSON"; then
        pass "uninstall removes tokenless entries, keeps user settings/hooks"
    else
        fail "uninstall left wrong state (entries=$entries)"
    fi
else
    fail "uninstall exited non-zero: $(cat "$SANDBOX/out6")"
fi

# --- Test 7: uninstall is idempotent ----------------------------------------
if ANOLISA_TARGET=workbuddy bash "$ADAPTER_DIR/workbuddy/scripts/uninstall.sh" >/dev/null 2>&1; then
    pass "uninstall idempotent"
else
    fail "second uninstall exited non-zero"
fi

# --- Test 8: invalid existing settings.json is never clobbered ---------------
export HOME="$SANDBOX/home-bad"
mkdir -p "$HOME/.codebuddy"
echo '{ not json' > "$HOME/.codebuddy/settings.json"
if ANOLISA_TARGET=workbuddy ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
    bash "$ADAPTER_DIR/workbuddy/scripts/install.sh" >/dev/null 2>&1; then
    fail "install accepted an invalid settings.json"
else
    if grep -qF '{ not json' "$HOME/.codebuddy/settings.json"; then
        pass "install refuses invalid settings.json without clobbering it"
    else
        fail "invalid settings.json was modified"
    fi
fi

# --- Test 9: detect.sh tri-state --------------------------------------------
export HOME="$SANDBOX/home-detect"
mkdir -p "$HOME"
ANOLISA_TARGET=workbuddy ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
    bash "$ADAPTER_DIR/workbuddy/scripts/detect.sh" >/dev/null 2>&1
detect_rc=$?
if [ "$detect_rc" -eq 2 ]; then
    pass "detect.sh exits 2 (missing prereqs) without WorkBuddy"
else
    fail "detect.sh unexpected exit code: $detect_rc"
fi

# --- Test 10: no stray temp files are left behind -----------------------------
leftovers="$(find "$SANDBOX/home-perms/.codebuddy" "$SANDBOX/home-perms-fresh/.codebuddy" \
    -maxdepth 1 -name '*.tmp' -print -quit 2>/dev/null)"
if [ -z "$leftovers" ]; then
    pass "no temp files left behind in .codebuddy"
else
    fail "temp file left behind: $leftovers"
fi

echo ""
echo "Results: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
