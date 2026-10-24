#!/usr/bin/env bash
# uninstall.sh — Remove agent-memory plugin via OpenClaw CLI + clean config.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-openclaw}"
COMPONENT="${ANOLISA_COMPONENT:-agent-memory}"
PLUGIN_ID="memory-anolisa"
OPENCLAW_HOME="${OPENCLAW_HOME:-$HOME/.openclaw}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-$OPENCLAW_HOME}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"
OPENCLAW_HOME="${OPENCLAW_HOME%/}"
OPENCLAW_BIN="${OPENCLAW_BIN:-openclaw}"
OPENCLAW_CFG="${OPENCLAW_STATE_DIR}/openclaw.json"

# Earlier installers recorded an explicit memory-core disable. Keep that record
# for the operator: enabling it here would also change the selected memory slot.
MEMORY_CORE_MARKER="${OPENCLAW_STATE_DIR}/.anolisa-memory-anolisa-disabled-memory-core"
if [ -f "$MEMORY_CORE_MARKER" ]; then
    echo "[${COMPONENT}] WARNING: a previous installer recorded a memory-core disable: ${MEMORY_CORE_MARKER}." >&2
    echo "[${COMPONENT}]          Review plugins.slots.memory before restoring it; 'plugins enable memory-core'" >&2
    echo "[${COMPONENT}]          also selects that backend. The marker is kept for manual recovery; see the user guide." >&2
fi

echo "[${COMPONENT}] Removing ${AGENT} plugin..."

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    # Preview before the first removal, in the branch the resolved state takes:
    # with a CLI the deregistration is the CLI's, without one the files are
    # removed directly, and the config rewrite follows either way.
    if command -v "$OPENCLAW_BIN" &>/dev/null; then
        echo "DRY-RUN: env -u OPENCLAW_HOME OPENCLAW_STATE_DIR=${OPENCLAW_STATE_DIR} ${OPENCLAW_BIN} plugins uninstall ${PLUGIN_ID} --force"
    else
        echo "DRY-RUN: rm -rf ${OPENCLAW_STATE_DIR}/plugins/${PLUGIN_ID} (no ${OPENCLAW_BIN} on PATH)"
        echo "DRY-RUN: rm -rf ${OPENCLAW_STATE_DIR}/extensions/memory-anolisa (no ${OPENCLAW_BIN} on PATH)"
    fi
    if [ -f "$OPENCLAW_CFG" ]; then
        # Mirror the real path's three branches (jq, python3, neither): the
        # preview may not promise an edit this host cannot make.
        if command -v jq &>/dev/null; then
            echo "DRY-RUN: drop ${PLUGIN_ID} from plugins.allow, plugins.entries and plugins.slots in ${OPENCLAW_CFG} (jq)"
        elif command -v python3 &>/dev/null; then
            echo "DRY-RUN: drop ${PLUGIN_ID} from plugins.allow, plugins.entries and plugins.slots in ${OPENCLAW_CFG} (python3)"
        else
            echo "DRY-RUN: WARN: neither jq nor python3 is on PATH — a real uninstall would leave ${PLUGIN_ID} referenced in ${OPENCLAW_CFG}; edit it manually"
        fi
    fi
    exit 0
fi

if ! command -v "$OPENCLAW_BIN" &>/dev/null; then
    echo "[${COMPONENT}] openclaw CLI not found (OPENCLAW_BIN=${OPENCLAW_BIN}) — removing plugin files manually."
    rm -rf "${OPENCLAW_STATE_DIR}/plugins/${PLUGIN_ID}" 2>/dev/null || true
    rm -rf "${OPENCLAW_STATE_DIR}/extensions/memory-anolisa" 2>/dev/null || true
else
    env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" plugins uninstall "$PLUGIN_ID" --force || true
fi

# Clean openclaw.json config entries (plugins.allow + plugins.entries + plugins.slots).
# Only this plugin's own keys go: a slot the operator moved to another backend
# is theirs.
if [ -f "$OPENCLAW_CFG" ]; then
    if command -v jq &>/dev/null; then
        jq '(.plugins.allow // [] | map(select(. != "'"$PLUGIN_ID"'"))) as $allow |
            (.plugins.entries // {} | del(.["'"$PLUGIN_ID"'"])) as $entries |
            (.plugins.slots // {} | to_entries | map(select(.value != "'"$PLUGIN_ID"'")) | from_entries) as $slots |
            .plugins.allow = $allow | .plugins.entries = $entries | .plugins.slots = $slots' \
            "$OPENCLAW_CFG" > "${OPENCLAW_CFG}.tmp" && mv "${OPENCLAW_CFG}.tmp" "$OPENCLAW_CFG"
        echo "[${COMPONENT}] Cleaned ${AGENT} config entries from openclaw.json (via jq)."
    elif command -v python3 &>/dev/null; then
        # Fallback when jq isn't installed: same edit with stdlib JSON.
        python3 - "$OPENCLAW_CFG" "$PLUGIN_ID" <<'PYEOF'
import json, sys
cfg_path, pid = sys.argv[1], sys.argv[2]
with open(cfg_path) as f:
    cfg = json.load(f)
plugins = cfg.setdefault("plugins", {})
plugins["allow"] = [x for x in plugins.get("allow", []) if x != pid]
plugins["entries"] = {k: v for k, v in plugins.get("entries", {}).items() if k != pid}
plugins["slots"] = {k: v for k, v in plugins.get("slots", {}).items() if v != pid}
with open(cfg_path + ".tmp", "w") as f:
    json.dump(cfg, f, indent=2)
import os
os.replace(cfg_path + ".tmp", cfg_path)
PYEOF
        echo "[${COMPONENT}] Cleaned ${AGENT} config entries from openclaw.json (via python3)."
    else
        echo "[${COMPONENT}] WARN: neither jq nor python3 found — openclaw.json may still" >&2
        echo "[${COMPONENT}]       reference '${PLUGIN_ID}'. Edit ${OPENCLAW_CFG} manually." >&2
    fi
fi

echo "[${COMPONENT}] ${AGENT} plugin removed."
