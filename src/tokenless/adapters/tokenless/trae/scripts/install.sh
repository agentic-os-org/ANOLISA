#!/usr/bin/env bash
# install.sh — Merge the Tokenless hook groups into Trae's global hooks.json.
#
# Trae (TraeCode) reads hooks from ~/.trae-cn/hooks.json (CN edition) and
# ~/.trae/hooks.json (international edition), merging every enabled source.
# There is no plugin-root substitution, so the hook commands are stamped with
# the absolute adapter directory at install time. Entries owned by Tokenless
# are identified by the TOKENLESS_AGENT_ID=trae marker, which keeps the merge
# idempotent and lets uninstall.sh remove exactly what this script adds.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-trae}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
PLUGIN_DIR="$ADAPTER_DIR/trae"
HOOKS_TEMPLATE="$PLUGIN_DIR/hooks/hooks.json"

TRAE_HOMES=()
for home in "$HOME/.trae-cn" "$HOME/.trae"; do
    [ -d "$home" ] && TRAE_HOMES+=("$home")
done
if [ ${#TRAE_HOMES[@]} -eq 0 ]; then
    echo "[${COMPONENT}] Trae not detected (no ~/.trae-cn or ~/.trae) — skipping ${AGENT} hook installation."
    exit 0
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "[${COMPONENT}] ERROR: python3 is required by the Tokenless hooks" >&2
    exit 1
fi
if [ ! -f "$HOOKS_TEMPLATE" ]; then
    echo "[${COMPONENT}] ERROR: Trae hooks template not found: ${HOOKS_TEMPLATE}" >&2
    exit 1
fi
if [ ! -f "$PLUGIN_DIR/hooks/run-hook.sh" ]; then
    echo "[${COMPONENT}] ERROR: hook dispatcher not found: ${PLUGIN_DIR}/hooks/run-hook.sh" >&2
    exit 1
fi

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    for home in "${TRAE_HOMES[@]}"; do
        echo "DRY-RUN: merge tokenless hook groups into ${home}/hooks.json"
    done
    exit 0
fi

for home in "${TRAE_HOMES[@]}"; do
    TRAE_HOME="$home" TOKENLESS_HOOKS_TEMPLATE="$HOOKS_TEMPLATE" \
    TOKENLESS_ADAPTER_DIR="$PLUGIN_DIR" python3 - <<'PYEOF'
import json
import os
import sys

MARKER = "TOKENLESS_AGENT_ID=trae"
PLACEHOLDER = "@TOKENLESS_ADAPTER_DIR@"

trae_home = os.environ["TRAE_HOME"]
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
config_path = os.path.join(trae_home, "hooks.json")

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

config.setdefault("version", 1)
config["hooks"] = hooks

tmp_path = config_path + ".tmp"
with open(tmp_path, "w", encoding="utf-8") as handle:
    json.dump(config, handle, ensure_ascii=False, indent=2)
    handle.write("\n")
os.replace(tmp_path, config_path)

with open(config_path, encoding="utf-8") as handle:
    verify = json.load(handle)
if MARKER not in json.dumps(verify.get("hooks", {})):
    print(f"tokenless hooks missing after merge: {config_path}", file=sys.stderr)
    sys.exit(1)
PYEOF
    echo "[${COMPONENT}] ${AGENT} hooks installed into ${home}/hooks.json."
done

echo "[${COMPONENT}] Restart Trae to load the hooks."
