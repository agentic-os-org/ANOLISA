#!/usr/bin/env bash
# uninstall.sh — Remove agent-memory through the host-owned plugin lifecycle.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-openclaw}"
COMPONENT="${ANOLISA_COMPONENT:-agent-memory}"
PLUGIN_ID="memory-anolisa"
OPENCLAW_HOME="${OPENCLAW_HOME:-$HOME/.openclaw}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-$OPENCLAW_HOME}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"
OPENCLAW_HOME="${OPENCLAW_HOME%/}"
OPENCLAW_BIN="${OPENCLAW_BIN:-openclaw}"

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
    if command -v "$OPENCLAW_BIN" &>/dev/null; then
        echo "DRY-RUN: env -u OPENCLAW_HOME OPENCLAW_STATE_DIR=${OPENCLAW_STATE_DIR} ${OPENCLAW_BIN} plugins uninstall ${PLUGIN_ID} --force"
    else
        echo "DRY-RUN: ${OPENCLAW_BIN} is not on PATH — a real uninstall would fail without changing plugin files or configuration."
    fi
    exit 0
fi

if ! command -v "$OPENCLAW_BIN" &>/dev/null; then
    echo "[${COMPONENT}] ERROR: openclaw CLI not found (OPENCLAW_BIN=${OPENCLAW_BIN}); uninstall not completed." >&2
    echo "[${COMPONENT}]        Plugin files and configuration were left untouched. Restore the CLI and re-run this script." >&2
    exit 1
fi

# OpenClaw owns both its install records and configuration routing (JSON5,
# $include and OPENCLAW_CONFIG_PATH). Rewriting openclaw.json after a successful
# uninstall can turn a writable include into unsupported sibling overrides.
UNINSTALL_RC=0
env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" \
    "$OPENCLAW_BIN" plugins uninstall "$PLUGIN_ID" --force || UNINSTALL_RC=$?
if [ "$UNINSTALL_RC" -ne 0 ]; then
    echo "[${COMPONENT}] ERROR: OpenClaw uninstall failed (rc=${UNINSTALL_RC}); no additional cleanup was attempted." >&2
    echo "[${COMPONENT}]        Check the CLI output, repair the reported problem and re-run this script." >&2
    exit "$UNINSTALL_RC"
fi

echo "[${COMPONENT}] ${AGENT} plugin removed."
