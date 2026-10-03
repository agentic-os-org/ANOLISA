#!/usr/bin/env bash
# uninstall.sh — Remove only the Tokenless-owned hook entries from Trae's
# global hooks.json. User-configured hooks are never touched.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-trae}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"

TRAE_HOMES=()
for home in "$HOME/.trae-cn" "$HOME/.trae"; do
    [ -d "$home" ] && TRAE_HOMES+=("$home")
done
if [ ${#TRAE_HOMES[@]} -eq 0 ]; then
    echo "[${COMPONENT}] Trae not detected (no ~/.trae-cn or ~/.trae) — nothing to uninstall."
    exit 0
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "[${COMPONENT}] ERROR: python3 is required to edit hooks.json" >&2
    exit 1
fi

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    for home in "${TRAE_HOMES[@]}"; do
        echo "DRY-RUN: remove tokenless hook entries from ${home}/hooks.json"
    done
    exit 0
fi

for home in "${TRAE_HOMES[@]}"; do
    TRAE_HOME="$home" python3 - <<'PYEOF'
import json
import os
import sys

MARKER = "TOKENLESS_AGENT_ID=trae"
trae_home = os.environ["TRAE_HOME"]
config_path = os.path.join(trae_home, "hooks.json")

if not os.path.exists(config_path):
    print(f"[tokenless] no hooks.json in {trae_home} — nothing to uninstall.")
    sys.exit(0)

with open(config_path, encoding="utf-8") as handle:
    raw = handle.read().strip()
if not raw:
    sys.exit(0)
try:
    config = json.loads(raw)
except json.JSONDecodeError as error:
    print(f"existing {config_path} is not valid JSON: {error}", file=sys.stderr)
    sys.exit(1)
if not isinstance(config, dict):
    print(f"unexpected root type in {config_path}", file=sys.stderr)
    sys.exit(1)

hooks = config.get("hooks")
if not isinstance(hooks, dict) or MARKER not in json.dumps(hooks):
    print(f"[tokenless] no tokenless hooks in {config_path} — nothing to uninstall.")
    sys.exit(0)


def is_tokenless_hook(entry):
    return (
        isinstance(entry, dict)
        and isinstance(entry.get("command"), str)
        and MARKER in entry["command"]
    )


for event in list(hooks.keys()):
    groups = hooks[event]
    if not isinstance(groups, list):
        continue
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
            kept.append(group)
    hooks[event] = kept

tmp_path = config_path + ".tmp"
with open(tmp_path, "w", encoding="utf-8") as handle:
    json.dump(config, handle, ensure_ascii=False, indent=2)
    handle.write("\n")
os.replace(tmp_path, config_path)
print(f"[tokenless] tokenless hooks removed from {config_path}.")
PYEOF
done

echo "[${COMPONENT}] ${AGENT} hooks uninstalled."
