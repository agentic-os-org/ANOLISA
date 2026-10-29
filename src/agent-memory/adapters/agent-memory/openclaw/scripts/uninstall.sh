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
# Retain the complete diagnostic: only the host's exact missing-plugin result
# is idempotent. Other errors must not be hidden by a substring match.
UNINSTALL_LOG="$(mktemp)"
trap 'rm -f "$UNINSTALL_LOG"' EXIT
set +e
env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" NO_COLOR=1 \
    "$OPENCLAW_BIN" plugins uninstall "$PLUGIN_ID" --force 2>&1 | tee "$UNINSTALL_LOG"
UNINSTALL_STATUS=("${PIPESTATUS[@]}")
set -e
UNINSTALL_RC="${UNINSTALL_STATUS[0]}"
if [ "$UNINSTALL_RC" -ne 0 ]; then
    DIAGNOSTIC="$(awk 'NF {gsub(/^[[:space:]]+|[[:space:]]+$/, ""); print tolower($0)}' "$UNINSTALL_LOG")"
    if [ "$UNINSTALL_RC" -eq 1 ] && [ "${UNINSTALL_STATUS[1]}" -eq 0 ] && \
        [ "$DIAGNOSTIC" = "plugin not found: ${PLUGIN_ID}" ]; then
        echo "[${COMPONENT}] ${PLUGIN_ID} is already unregistered."
    else
        echo "[${COMPONENT}] ERROR: OpenClaw uninstall failed (rc=${UNINSTALL_RC}); no additional cleanup was attempted." >&2
        echo "[${COMPONENT}]        Check the CLI output, repair the reported problem and re-run this script." >&2
        exit "$UNINSTALL_RC"
    fi
elif [ "${UNINSTALL_STATUS[1]}" -ne 0 ]; then
    echo "[${COMPONENT}] ERROR: could not capture the complete OpenClaw uninstall result." >&2
    exit 1
fi

# A successful host command can still warn that directory removal failed.
# Report known residual paths without deleting untracked files or rolling back
# configuration changes already made by the host. Include dangling symlinks.
RESIDUE=0
for PLUGIN_PATH in "${OPENCLAW_STATE_DIR}/extensions/${PLUGIN_ID}" \
    "${OPENCLAW_STATE_DIR}/plugins/${PLUGIN_ID}"; do
    if [ -e "$PLUGIN_PATH" ] || [ -L "$PLUGIN_PATH" ]; then
        echo "[${COMPONENT}] ERROR: plugin path remains after host uninstall: ${PLUGIN_PATH}" >&2
        RESIDUE=1
    fi
done
if [ "$RESIDUE" -ne 0 ]; then
    echo "[${COMPONENT}]        Inspect the remaining paths and host output; no additional cleanup was attempted." >&2
    exit 1
fi

echo "[${COMPONENT}] ${AGENT} plugin unregistered; no plugin paths remain under extensions/ or plugins/."
