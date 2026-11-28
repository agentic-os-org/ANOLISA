#!/usr/bin/env bash
# Regression tests for the Trae (TraeCode) adapter install lifecycle.
#
# Trae has no plugin system for hooks: install.sh merges the tokenless hook
# groups into the edition's global hooks.json (~/.trae-cn CN edition,
# ~/.trae international edition). The merge must be idempotent, must never
# clobber user-configured hooks, must preserve the host config file's access
# mode and managed path, must emit a dispatcher path the host shell cannot
# re-expand, and uninstall.sh must remove exactly the tokenless-owned entries.
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
PLACEHOLDER="@TOKENLESS_DISPATCHER@"

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

file_mode() {
    # Octal access mode, e.g. 600. Portable across stat flavours.
    python3 -c 'import os,stat,sys
print(format(stat.S_IMODE(os.stat(sys.argv[1]).st_mode), "o"))' "$1"
}

file_sha() {
    python3 -c 'import hashlib,sys
print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$1"
}

json_valid() { python3 -m json.tool "$1" >/dev/null 2>&1; }

run_install() {
    local adapter_dir="${1:-$ADAPTER_DIR}"
    ANOLISA_TARGET=trae ANOLISA_ADAPTER_DIR="$adapter_dir" \
        bash "$adapter_dir/trae/scripts/install.sh"
}

run_uninstall() {
    ANOLISA_TARGET=trae ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
        bash "$ADAPTER_DIR/trae/scripts/uninstall.sh"
}

# --- Test 1: source template is valid JSON and carries the marker ---------
if json_valid "$ADAPTER_DIR/trae/hooks/hooks.json" \
    && grep -qF "$MARKER" "$ADAPTER_DIR/trae/hooks/hooks.json" \
    && grep -qF "$PLACEHOLDER" "$ADAPTER_DIR/trae/hooks/hooks.json"; then
    pass "hooks template is valid JSON with marker and dispatcher placeholder"
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

if run_install >"$SANDBOX/out3" 2>&1; then
    pass "install into CN edition exits 0"
else
    fail "install into CN edition failed: $(cat "$SANDBOX/out3")"
fi

HOOKS_JSON="$HOME/.trae-cn/hooks.json"
python3 - "$HOOKS_JSON" "$ADAPTER_DIR" <<'PYEOF' && pass "merge preserves user hooks and stamps adapter dir" || fail "merge content wrong"
import json
import shlex
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
assert all("@TOKENLESS_DISPATCHER@" not in c for c in tokenless_cmds), "placeholder left unstamped"
dispatcher = f"{adapter_dir}/trae/hooks/run-hook.sh"
for command in tokenless_cmds:
    # The dispatcher must survive shell parsing as one literal word: the host
    # runs this string through a shell, and the adapter root above contains a
    # space.
    words = shlex.split(command)
    assert len(words) == 4, f"expected 4 shell words, got {words}"
    assert words[0] == "TOKENLESS_AGENT_ID=trae", words
    assert words[1] == "bash", words
    assert words[2] == dispatcher, f"dispatcher word {words[2]!r} != {dispatcher!r}"
assert any(c.endswith("rewrite_hook.py") for c in tokenless_cmds)
assert any(c.endswith("tool_ready_hook.sh") for c in tokenless_cmds)
assert any(c.endswith("compress_response_hook.py") for c in tokenless_cmds)

matchers = {g.get("matcher"): g for g in hooks["PreToolUse"]}
assert "RunCommand" in matchers, "rewrite hook must match RunCommand"
PYEOF

# --- Test 4: install is idempotent -----------------------------------------
if run_install >/dev/null 2>&1; then
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
if run_install >/dev/null 2>&1 \
    && [ "$(count_tokenless_entries "$HOME/.trae-cn/hooks.json")" = "3" ] \
    && [ "$(count_tokenless_entries "$HOME/.trae/hooks.json")" = "3" ]; then
    pass "install covers both CN and international editions"
else
    fail "install did not cover both editions"
fi

# --- Test 6: uninstall removes only tokenless entries -----------------------
export HOME="$SANDBOX/home-cn"
if run_uninstall >"$SANDBOX/out6" 2>&1; then
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
if run_uninstall >/dev/null 2>&1; then
    pass "uninstall idempotent"
else
    fail "second uninstall exited non-zero"
fi

# --- Test 8: invalid existing hooks.json is never clobbered ------------------
export HOME="$SANDBOX/home-bad"
mkdir -p "$HOME/.trae-cn"
echo '{ not json' > "$HOME/.trae-cn/hooks.json"
if run_install >/dev/null 2>&1; then
    fail "install accepted an invalid hooks.json"
else
    if grep -qF '{ not json' "$HOME/.trae-cn/hooks.json"; then
        pass "install refuses invalid hooks.json without clobbering it"
    else
        fail "invalid hooks.json was modified"
    fi
fi

# --- Test 9: detect.sh is fail-open and still reports ------------------------
export HOME="$SANDBOX/home-detect"
mkdir -p "$HOME"
ANOLISA_TARGET=trae ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
    bash "$ADAPTER_DIR/trae/scripts/detect.sh" >"$SANDBOX/out9" 2>&1
detect_rc=$?
if [ "$detect_rc" -eq 0 ] && grep -qF "trae: missing prerequisites" "$SANDBOX/out9"; then
    pass "detect.sh exits 0 without Trae and names the missing prerequisites"
else
    fail "detect.sh without Trae: rc=$detect_rc, output: $(tail -3 "$SANDBOX/out9")"
fi

# --- Test 10: install preserves the access mode of an existing config --------
export HOME="$SANDBOX/home-mode"
mkdir -p "$HOME/.trae-cn"
echo '{"version":1,"hooks":{}}' > "$HOME/.trae-cn/hooks.json"
chmod 0600 "$HOME/.trae-cn/hooks.json"
( umask 022; run_install >/dev/null 2>&1 )
mode="$(file_mode "$HOME/.trae-cn/hooks.json")"
if [ "$mode" = "600" ]; then
    pass "install preserves a 0600 hooks.json mode under umask 022"
else
    fail "install changed hooks.json mode to $mode (expected 600)"
fi

# --- Test 11: uninstall preserves the access mode too ------------------------
chmod 0600 "$HOME/.trae-cn/hooks.json"
( umask 022; run_uninstall >/dev/null 2>&1 )
mode="$(file_mode "$HOME/.trae-cn/hooks.json")"
if [ "$mode" = "600" ]; then
    pass "uninstall preserves a 0600 hooks.json mode under umask 022"
else
    fail "uninstall changed hooks.json mode to $mode (expected 600)"
fi

# --- Test 12: a brand-new config still gets the umask-derived mode -----------
export HOME="$SANDBOX/home-new"
mkdir -p "$HOME/.trae-cn"
( umask 022; run_install >/dev/null 2>&1 )
mode="$(file_mode "$HOME/.trae-cn/hooks.json")"
if [ "$mode" = "644" ]; then
    pass "install creates a new hooks.json with the umask-derived mode (644)"
else
    fail "new hooks.json mode is $mode (expected 644 under umask 022)"
fi

# --- Test 13: a dotfiles-managed symlink survives install --------------------
export HOME="$SANDBOX/home-link"
mkdir -p "$HOME/.trae-cn" "$SANDBOX/dotfiles"
LINK_TARGET="$SANDBOX/dotfiles/trae-hooks.json"
cat > "$LINK_TARGET" <<'USERJSON'
{
  "version": 1,
  "hooks": {
    "Stop": [
      { "hooks": [ { "type": "command", "command": "echo user-stop-hook" } ] }
    ]
  }
}
USERJSON
chmod 0600 "$LINK_TARGET"
ln -s "$LINK_TARGET" "$HOME/.trae-cn/hooks.json"

if run_install >"$SANDBOX/out13" 2>&1; then
    if [ -L "$HOME/.trae-cn/hooks.json" ] \
        && [ "$(readlink "$HOME/.trae-cn/hooks.json")" = "$LINK_TARGET" ] \
        && [ "$(count_tokenless_entries "$LINK_TARGET")" = "3" ] \
        && [ "$(file_mode "$LINK_TARGET")" = "600" ] \
        && grep -qF "user-stop-hook" "$LINK_TARGET"; then
        pass "install writes through a dotfiles symlink, keeping link, target and mode"
    else
        fail "install broke the managed symlink or its target/mode"
    fi
else
    fail "install into a symlinked hooks.json failed: $(cat "$SANDBOX/out13")"
fi

# --- Test 14: the same symlink survives uninstall ----------------------------
if run_uninstall >"$SANDBOX/out14" 2>&1; then
    if [ -L "$HOME/.trae-cn/hooks.json" ] \
        && [ "$(readlink "$HOME/.trae-cn/hooks.json")" = "$LINK_TARGET" ] \
        && [ "$(count_tokenless_entries "$LINK_TARGET")" = "0" ] \
        && [ "$(file_mode "$LINK_TARGET")" = "600" ] \
        && grep -qF "user-stop-hook" "$LINK_TARGET"; then
        pass "uninstall writes through a dotfiles symlink, keeping link, target and mode"
    else
        fail "uninstall broke the managed symlink or its target/mode"
    fi
else
    fail "uninstall of a symlinked hooks.json failed: $(cat "$SANDBOX/out14")"
fi

# --- Test 15: user hooks that mention the marker survive install -------------
export HOME="$SANDBOX/home-marker"
mkdir -p "$HOME/.trae-cn"
cat > "$HOME/.trae-cn/hooks.json" <<'USERJSON'
{
  "version": 1,
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Write",
        "hooks": [
          { "type": "command", "command": "echo TOKENLESS_AGENT_ID=trae user-pre-hook", "timeout": 5 }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "echo TOKENLESS_AGENT_ID=trae user-stop-hook" }
        ]
      }
    ]
  }
}
USERJSON
MARKER_JSON="$HOME/.trae-cn/hooks.json"

if run_install >"$SANDBOX/out15" 2>&1 \
    && grep -qF "user-pre-hook" "$MARKER_JSON" \
    && grep -qF "user-stop-hook" "$MARKER_JSON" \
    && [ "$(count_tokenless_entries "$MARKER_JSON")" = "5" ]; then
    pass "install keeps user hooks that merely mention the marker (3 adapter + 2 user)"
else
    fail "install dropped a user hook that mentions the marker: $(cat "$SANDBOX/out15" 2>/dev/null)"
fi

# --- Test 16: the same user hooks survive uninstall --------------------------
if run_uninstall >"$SANDBOX/out16" 2>&1; then
    python3 - "$MARKER_JSON" <<'PYEOF' && pass "uninstall removes only adapter-owned commands, keeps marker-mentioning user groups" || fail "uninstall touched a user group that mentions the marker"
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    hooks = json.load(handle)["hooks"]

stop_commands = [h.get("command") for g in hooks.get("Stop", []) for h in g.get("hooks", [])]
assert stop_commands == ["echo TOKENLESS_AGENT_ID=trae user-stop-hook"], stop_commands

pre_commands = [h.get("command") for g in hooks.get("PreToolUse", []) for h in g.get("hooks", [])]
assert pre_commands == ["echo TOKENLESS_AGENT_ID=trae user-pre-hook"], pre_commands
PYEOF
else
    fail "uninstall exited non-zero: $(cat "$SANDBOX/out16")"
fi

# --- Test 17: uninstall leaves an unowned config byte-for-byte alone ---------
export HOME="$SANDBOX/home-untouched"
mkdir -p "$HOME/.trae-cn"
printf '{ "version": 1, "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "echo TOKENLESS_AGENT_ID=trae" } ] } ] } }' \
    > "$HOME/.trae-cn/hooks.json"
before="$(file_sha "$HOME/.trae-cn/hooks.json")"
if run_uninstall >/dev/null 2>&1 && [ "$(file_sha "$HOME/.trae-cn/hooks.json")" = "$before" ]; then
    pass "uninstall does not rewrite a config with no adapter-owned entries"
else
    fail "uninstall rewrote a config it owns nothing in"
fi

# --- Test 18: the generated command executes from a shell-hostile root -------
# The dispatcher path lands inside a command string the host hands to a shell.
# An adapter root with a literal `$budget` segment must not be expanded.
NASTY_ADAPTER_DIR="$SANDBOX/build \$budget 'n' root"
mkdir -p "$NASTY_ADAPTER_DIR"
cp -R "$SOURCE_ADAPTER_DIR/trae" "$NASTY_ADAPTER_DIR/"
rm -f "$NASTY_ADAPTER_DIR/trae/hooks/run-hook.sh"
cat > "$NASTY_ADAPTER_DIR/trae/hooks/run-hook.sh" <<'STUB'
#!/usr/bin/env bash
echo "DISPATCHER-REACHED script=${1:-none}"
STUB
chmod 0755 "$NASTY_ADAPTER_DIR/trae/hooks/run-hook.sh"

export HOME="$SANDBOX/home-exec"
mkdir -p "$HOME/.trae"
if run_install "$NASTY_ADAPTER_DIR" >"$SANDBOX/out18" 2>&1; then
    rewrite_cmd="$(python3 -c '
import json, shlex, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    config = json.load(handle)
for group in config["hooks"]["PreToolUse"]:
    for hook in group.get("hooks", []):
        command = hook.get("command", "")
        if shlex.split(command)[-1:] == ["rewrite_hook.py"]:
            print(command)
            raise SystemExit(0)
raise SystemExit(1)
' "$HOME/.trae/hooks.json")"
    exec_out="$(printf '%s' '{"session_id":"s1","tool_name":"RunCommand","tool_input":{"command":"ls"}}' \
        | bash -c "$rewrite_cmd" 2>"$SANDBOX/err18")"
    exec_rc=$?
    if [ "$exec_rc" -eq 0 ] && [ "$exec_out" = "DISPATCHER-REACHED script=rewrite_hook.py" ]; then
        pass "generated command executes literally from an adapter root containing \$budget"
    else
        fail "generated command did not execute (rc=$exec_rc out='$exec_out' err='$(cat "$SANDBOX/err18")')"
    fi
else
    fail "install from a shell-hostile adapter root failed: $(cat "$SANDBOX/out18")"
fi

# --- Test 19: detect.sh accepts the bundled rtk runtime ----------------------
# Packaging ships no standalone `rtk`; RTK runs as `tokenless rtk`. The
# supported layout (adapter tree staged + tokenless on PATH) must report ready.
export HOME="$SANDBOX/home-detect-ready"
mkdir -p "$HOME/.trae-cn" "$HOME/.local/bin" \
    "$HOME/.local/share/anolisa/adapters/tokenless/common/hooks"
write_fake_tokenless() {
    # $1 = 0 to answer `rtk --version`, 1 to fail everything
    if [ "$1" = "0" ]; then
        cat > "$HOME/.local/bin/tokenless" <<'FAKE'
#!/usr/bin/env bash
if [ "${1:-}" = "rtk" ] && [ "${2:-}" = "--version" ]; then
    echo "0.49.0"
    exit 0
fi
exit 1
FAKE
    else
        cat > "$HOME/.local/bin/tokenless" <<'FAKE'
#!/usr/bin/env bash
exit 1
FAKE
    fi
    chmod 0755 "$HOME/.local/bin/tokenless"
}
run_detect() {
    ANOLISA_TARGET=trae ANOLISA_ADAPTER_DIR="$ADAPTER_DIR" \
        bash "$ADAPTER_DIR/trae/scripts/detect.sh"
}

write_fake_tokenless 0
run_install >/dev/null 2>&1
run_detect >"$SANDBOX/out19" 2>&1
detect_rc=$?
if [ "$detect_rc" -eq 0 ] \
    && grep -qF "bundled: tokenless rtk 0.49.0" "$SANDBOX/out19" \
    && grep -qF "trae: ready" "$SANDBOX/out19"; then
    pass "detect.sh reports ready with the bundled rtk runtime and no standalone rtk"
else
    fail "detect.sh with bundled rtk exited $detect_rc: $(tail -4 "$SANDBOX/out19")"
fi

# --- Test 20: a broken rtk runtime is reported, still fail-open --------------
write_fake_tokenless 1
run_detect >"$SANDBOX/out20" 2>&1
detect_rc=$?
if [ "$detect_rc" -eq 0 ] \
    && grep -qF "rtk runtime (tokenless rtk)" "$SANDBOX/out20" \
    && grep -qF "trae: missing prerequisites" "$SANDBOX/out20"; then
    pass "detect.sh names the rtk runtime when tokenless rtk fails, and still exits 0"
else
    fail "detect.sh with a broken rtk runtime exited $detect_rc: $(tail -4 "$SANDBOX/out20")"
fi

# --- Test 21: install rejects a dangling hooks.json symlink ------------------
# A stale dotfiles link whose target is missing must not be adopted: creating the
# target would silently make the adapter own a file the user never chose.
export HOME="$SANDBOX/home-dangling"
mkdir -p "$HOME/.trae-cn" "$SANDBOX/dotfiles-dangling"
DANGLING_TARGET="$SANDBOX/dotfiles-dangling/missing.json"
ln -s "$DANGLING_TARGET" "$HOME/.trae-cn/hooks.json"

if run_install >"$SANDBOX/out21" 2>&1; then
    fail "install accepted a dangling hooks.json symlink"
elif [ -L "$HOME/.trae-cn/hooks.json" ] \
    && [ "$(readlink "$HOME/.trae-cn/hooks.json")" = "$DANGLING_TARGET" ] \
    && [ ! -e "$DANGLING_TARGET" ] \
    && [ -z "$(ls -A "$SANDBOX/dotfiles-dangling")" ]; then
    pass "install rejects a dangling hooks.json symlink, leaving link and missing target untouched"
else
    fail "install created the missing target, replaced the link, or left a temporary behind"
fi

# uninstall must be equally inert: nothing is registered, nothing is written.
if run_uninstall >"$SANDBOX/out21b" 2>&1 \
    && [ -L "$HOME/.trae-cn/hooks.json" ] \
    && [ "$(readlink "$HOME/.trae-cn/hooks.json")" = "$DANGLING_TARGET" ] \
    && [ ! -e "$DANGLING_TARGET" ] \
    && [ -z "$(ls -A "$SANDBOX/dotfiles-dangling")" ]; then
    pass "uninstall leaves a dangling hooks.json symlink and its missing target untouched"
else
    fail "uninstall touched a dangling hooks.json symlink: $(tail -3 "$SANDBOX/out21b" 2>/dev/null)"
fi

# --- Test 22: readiness is ownership-based, not marker-based -----------------
# A user hook that merely mentions the marker must not make the adapter look
# registered. Same supported layout as Test 19, but nothing adapter-owned.
export HOME="$SANDBOX/home-marker-detect"
mkdir -p "$HOME/.trae-cn" "$HOME/.local/bin" \
    "$HOME/.local/share/anolisa/adapters/tokenless/common/hooks"
write_fake_tokenless 0
cat > "$HOME/.trae-cn/hooks.json" <<'USERJSON'
{
  "version": 1,
  "hooks": {
    "Stop": [
      { "hooks": [ { "type": "command", "command": "echo TOKENLESS_AGENT_ID=trae" } ] }
    ]
  }
}
USERJSON

run_detect >"$SANDBOX/out22" 2>&1
detect_rc=$?
if [ "$detect_rc" -eq 0 ] \
    && grep -qF "not installed (no adapter-owned entries)" "$SANDBOX/out22" \
    && grep -qF "trae: not installed (ready to install)" "$SANDBOX/out22"; then
    pass "detect.sh does not count a marker-bearing user hook as registered"
else
    fail "detect.sh mis-read a marker-bearing user hook: rc=$detect_rc $(tail -4 "$SANDBOX/out22")"
fi

# --- Test 23: the same edition reads as ready once really installed ----------
if run_install >"$SANDBOX/out23" 2>&1; then
    run_detect >"$SANDBOX/out23d" 2>&1
    detect_rc=$?
    if [ "$detect_rc" -eq 0 ] \
        && grep -qF "installed (3 adapter-owned entries)" "$SANDBOX/out23d" \
        && grep -qF "trae: ready" "$SANDBOX/out23d" \
        && grep -qF "echo TOKENLESS_AGENT_ID=trae" "$HOME/.trae-cn/hooks.json"; then
        pass "detect.sh reports ready after a real install and the user Stop hook survives it"
    else
        fail "detect.sh after install: rc=$detect_rc $(tail -4 "$SANDBOX/out23d")"
    fi
else
    fail "install for the readiness test failed: $(tail -3 "$SANDBOX/out23")"
fi

# --- Test 24: template and lifecycle stay in agreement -----------------------
python3 - "$ADAPTER_DIR/trae/hooks/hooks.json" \
    "$ADAPTER_DIR/trae/scripts/lifecycle.py" \
    "$ADAPTER_DIR/trae/scripts/install.sh" \
    "$ADAPTER_DIR/trae/scripts/uninstall.sh" \
    "$ADAPTER_DIR/trae/scripts/detect.sh" <<'PYEOF' && pass "template stays inside what lifecycle.py owns, and all three scripts delegate to it" || fail "template or scripts drifted from lifecycle.py"
import ast
import json
import re
import shlex
import sys

template_path, lifecycle_path, install_path, uninstall_path, detect_path = sys.argv[1:6]

with open(template_path, encoding="utf-8") as handle:
    template = json.load(handle)
events = set(template.get("hooks", {}))
scripts = {
    shlex.split(hook["command"])[-1]
    for groups in template["hooks"].values()
    for group in groups
    for hook in group.get("hooks", [])
}

with open(lifecycle_path, encoding="utf-8") as handle:
    source = handle.read()


def literal(pattern, name):
    match = re.search(pattern, source, re.DOTALL)
    assert match, f"{name} not found in lifecycle.py"
    return ast.literal_eval(match.group(1))


owned = literal(r"OWNED_SCRIPTS = frozenset\(\s*(\{.*?\})\s*\)", "OWNED_SCRIPTS")
managed = literal(r"MANAGED_EVENTS = (\(.*?\))\n", "MANAGED_EVENTS")

assert scripts == set(owned), (sorted(scripts), sorted(owned))
assert events <= set(managed), (sorted(events), sorted(managed))

# The ownership predicate and the atomic writer must live in exactly one place.
for path in (install_path, uninstall_path, detect_path):
    with open(path, encoding="utf-8") as handle:
        script = handle.read()
    assert "lifecycle.py" in script, f"{path} no longer delegates to lifecycle.py"
    assert "TOKENLESS_AGENT_ID=trae" not in script, f"{path} re-implements marker matching"
    assert "os.replace" not in script, f"{path} re-implements the config writer"
PYEOF

# --- Test 25: readiness and removal share one event scope --------------------
# An adapter-owned command a user moved into `Stop` is not registered: the host
# only runs the rewrite hook from PreToolUse, so it never fires on RunCommand,
# and uninstall does not touch Stop either. detect must agree with uninstall
# instead of reporting ready for a config that neither runs nor removes.
export HOME="$SANDBOX/home-scope"
mkdir -p "$HOME/.trae-cn" "$HOME/.local/bin" \
    "$HOME/.local/share/anolisa/adapters/tokenless/common/hooks"
write_fake_tokenless 0
run_install >"$SANDBOX/out25i" 2>&1
python3 - "$HOME/.trae-cn/hooks.json" <<'PYEOF'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    config = json.load(handle)
rewrite = None
for group in config["hooks"].get("PreToolUse", []):
    for hook in group.get("hooks", []):
        if hook.get("command", "").endswith("rewrite_hook.py"):
            rewrite = hook
assert rewrite is not None, "install generated no rewrite command"
config["hooks"] = {"Stop": [{"hooks": [rewrite]}]}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(config, handle, indent=2)
PYEOF

run_detect >"$SANDBOX/out25" 2>&1
detect_rc=$?
scope_sha_before="$(file_sha "$HOME/.trae-cn/hooks.json")"
run_uninstall >"$SANDBOX/out25u" 2>&1
if [ "$detect_rc" -eq 0 ] \
    && grep -qF "not installed (no adapter-owned entries)" "$SANDBOX/out25" \
    && grep -qF "trae: not installed (ready to install)" "$SANDBOX/out25" \
    && [ "$(file_sha "$HOME/.trae-cn/hooks.json")" = "$scope_sha_before" ] \
    && grep -qF "rewrite_hook.py" "$HOME/.trae-cn/hooks.json"; then
    pass "an owned command moved into Stop reads as unregistered, matching what uninstall touches"
else
    fail "detect and uninstall disagree about a Stop-only config: rc=$detect_rc $(tail -4 "$SANDBOX/out25")"
fi

# --- Test 26: basename invocation resolves the script directory --------------
# `cd scripts && bash install.sh` leaves no slash in BASH_SOURCE[0], so a
# pattern-expansion dirname would keep the filename and fail the cd. All three
# entry points must work invoked that way, with no ANOLISA_ADAPTER_DIR help.
export HOME="$SANDBOX/home-basename"
mkdir -p "$HOME/.trae-cn"
BN_HOOKS="$HOME/.trae-cn/hooks.json"

( cd "$ADAPTER_DIR/trae/scripts" && bash install.sh ) >"$SANDBOX/out26-install" 2>&1
install_rc=$?
( cd "$ADAPTER_DIR/trae/scripts" && bash detect.sh ) >"$SANDBOX/out26-detect" 2>&1
detect_rc=$?
owned_after_install="$(count_tokenless_entries "$BN_HOOKS")"

( cd "$ADAPTER_DIR/trae/scripts" && bash uninstall.sh ) >"$SANDBOX/out26-uninstall" 2>&1
uninstall_rc=$?

if [ "$install_rc" -eq 0 ] && [ "$owned_after_install" = "3" ] \
    && [ "$detect_rc" -eq 0 ] \
    && grep -qE "trae: (ready|not installed \(ready to install\)|missing prerequisites)" "$SANDBOX/out26-detect" \
    && [ "$uninstall_rc" -eq 0 ] \
    && [ "$(count_tokenless_entries "$BN_HOOKS")" = "0" ]; then
    pass "install/detect/uninstall all work when invoked by basename from inside scripts/"
else
    fail "basename invocation regressed: install rc=$install_rc (owned=$owned_after_install), detect rc=$detect_rc, uninstall rc=$uninstall_rc; $(head -1 "$SANDBOX/out26-install")"
fi

echo ""
echo "Results: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
