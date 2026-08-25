#!/usr/bin/env bash
# install.sh — Merge the Tokenless hook groups into WorkBuddy's user-level
# settings.json (~/.codebuddy/settings.json).
#
# WorkBuddy desktop, CodeBuddy Code CLI and WorkBuddy Enterprise share the
# .codebuddy settings protocol: a "hooks" key with Claude Code-shaped matcher
# groups, merged across scopes. There is no plugin-root substitution, so the
# hook commands are stamped with the absolute adapter directory at install
# time. Entries owned by Tokenless are identified by the
# TOKENLESS_AGENT_ID=workbuddy marker, which keeps the merge idempotent and
# lets uninstall.sh remove exactly what this script adds.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-workbuddy}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
PLUGIN_DIR="$ADAPTER_DIR/workbuddy"
HOOKS_TEMPLATE="$PLUGIN_DIR/hooks/hooks.json"
CODEBUDDY_HOME="${CODEBUDDY_HOME:-$HOME/.codebuddy}"

if [ ! -d "$CODEBUDDY_HOME" ]; then
    echo "[${COMPONENT}] WorkBuddy/CodeBuddy not detected (no ${CODEBUDDY_HOME}) — skipping ${AGENT} hook installation."
    exit 0
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "[${COMPONENT}] ERROR: python3 is required by the Tokenless hooks" >&2
    exit 1
fi
if [ ! -f "$HOOKS_TEMPLATE" ]; then
    echo "[${COMPONENT}] ERROR: WorkBuddy hooks template not found: ${HOOKS_TEMPLATE}" >&2
    exit 1
fi
if [ ! -f "$PLUGIN_DIR/hooks/run-hook.sh" ]; then
    echo "[${COMPONENT}] ERROR: hook dispatcher not found: ${PLUGIN_DIR}/hooks/run-hook.sh" >&2
    exit 1
fi

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    echo "DRY-RUN: merge tokenless hook groups into ${CODEBUDDY_HOME}/settings.json"
    exit 0
fi

CODEBUDDY_HOME="$CODEBUDDY_HOME" TOKENLESS_HOOKS_TEMPLATE="$HOOKS_TEMPLATE" \
TOKENLESS_ADAPTER_DIR="$PLUGIN_DIR" python3 - <<'PYEOF'
import json
import os
import stat
import sys
import tempfile

MARKER = "TOKENLESS_AGENT_ID=workbuddy"
PLACEHOLDER = "@TOKENLESS_ADAPTER_DIR@"

codebuddy_home = os.environ["CODEBUDDY_HOME"]
adapter_dir = os.environ["TOKENLESS_ADAPTER_DIR"]

with open(os.environ["TOKENLESS_HOOKS_TEMPLATE"], encoding="utf-8") as handle:
    template = json.load(handle)


def stamp(value):
    if isinstance(value, str):
        return value.replace(PLACEHOLDER, adapter_dir)
    if isinstance(value, list):
        return [stamp(item) for item in value]
    if isinstance(value, dict):
        return {key: stamp(item) for key, item in value.items()}
    return value


new_groups = stamp(template.get("hooks", {}))
config_path = os.path.join(codebuddy_home, "settings.json")

config = {}
if os.path.exists(config_path):
    with open(config_path, encoding="utf-8") as handle:
        raw = handle.read().strip()
    if raw:
        try:
            config = json.loads(raw)
        except json.JSONDecodeError as error:
            print(f"existing {config_path} is not valid JSON: {error}", file=sys.stderr)
            sys.exit(1)
if not isinstance(config, dict):
    print(f"unexpected root type in {config_path}", file=sys.stderr)
    sys.exit(1)

hooks = config.get("hooks")
if hooks is None:
    hooks = {}
if not isinstance(hooks, dict):
    print(f"unexpected hooks type in {config_path}", file=sys.stderr)
    sys.exit(1)


def is_tokenless_hook(entry):
    return (
        isinstance(entry, dict)
        and isinstance(entry.get("command"), str)
        and MARKER in entry["command"]
    )


def strip_tokenless(groups):
    kept = []
    for group in groups:
        if not isinstance(group, dict):
            kept.append(group)
            continue
        inner = group.get("hooks")
        if not isinstance(inner, list):
            kept.append(group)
            continue
        remaining = [entry for entry in inner if not is_tokenless_hook(entry)]
        if remaining:
            pruned = dict(group)
            pruned["hooks"] = remaining
            kept.append(pruned)
        elif not any(is_tokenless_hook(entry) for entry in inner):
            # A user group without hooks stays untouched; a group that held only
            # Tokenless hooks is dropped.
            kept.append(group)
    return kept


for event, additions in new_groups.items():
    existing = hooks.get(event, [])
    if not isinstance(existing, list):
        print(f"unexpected {event} type in {config_path}", file=sys.stderr)
        sys.exit(1)
    hooks[event] = strip_tokenless(existing) + additions

config["hooks"] = hooks

# The .codebuddy home can carry credentials: CodeBuddy officially allows
# CODEBUDDY_API_KEY and auth tokens in settings.json.env, and
# settings.json itself may hold sensitive keys. Rewriting must never
# loosen the existing file mode (a default-mode temp file under umask
# 022 would turn 0600 into 0644 on replace): stage a unique 0600 temp
# file in the same directory, restore the original mode, then replace.
existing_mode = None
if os.path.exists(config_path):
    existing_mode = stat.S_IMODE(os.stat(config_path).st_mode)
fd, tmp_path = tempfile.mkstemp(
    dir=codebuddy_home, prefix=".settings.json.", suffix=".tmp"
)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(config, handle, ensure_ascii=False, indent=2)
        handle.write("\n")
    os.chmod(tmp_path, 0o600 if existing_mode is None else existing_mode)
    os.replace(tmp_path, config_path)
except BaseException:
    try:
        os.unlink(tmp_path)
    except OSError:
        pass
    raise

with open(config_path, encoding="utf-8") as handle:
    verify = json.load(handle)
if MARKER not in json.dumps(verify.get("hooks", {})):
    print(f"tokenless hooks missing after merge: {config_path}", file=sys.stderr)
    sys.exit(1)
PYEOF

echo "[${COMPONENT}] ${AGENT} hooks installed into ${CODEBUDDY_HOME}/settings.json."
echo "[${COMPONENT}] Restart WorkBuddy/CodeBuddy to load the hooks."
echo "[${COMPONENT}] Note: the CodeBuddy CLI /hooks panel may ask you to review externally added hooks before they take effect."
