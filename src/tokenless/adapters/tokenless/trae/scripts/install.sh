#!/usr/bin/env bash
# install.sh — Merge the Tokenless hook groups into Trae's global hooks.json.
#
# Trae (TraeCode) reads hooks from ~/.trae-cn/hooks.json (CN edition) and
# ~/.trae/hooks.json (international edition), merging every enabled source.
# There is no plugin-root substitution, so the hook commands are stamped with
# the absolute dispatcher path at install time.
#
# The merge itself lives in lifecycle.py, which is also what uninstall.sh and
# detect.sh use: ownership of a hook entry, preservation of the managed config
# path and its access mode, and shell-quoting of the dispatcher path are decided
# in exactly one place so the three scripts cannot drift apart.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-trae}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
# `dirname`, not ${BASH_SOURCE[0]%/*}: a basename invocation (`bash install.sh`
# from inside this directory) has no slash to strip, so the pattern expansion
# would leave the filename and `cd` into it would fail.
SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
PLUGIN_DIR="$ADAPTER_DIR/trae"
HOOKS_TEMPLATE="$PLUGIN_DIR/hooks/hooks.json"
DISPATCHER="$PLUGIN_DIR/hooks/run-hook.sh"

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
if [ ! -f "$DISPATCHER" ]; then
    echo "[${COMPONENT}] ERROR: hook dispatcher not found: ${DISPATCHER}" >&2
    exit 1
fi
if [ ! -f "$SCRIPT_DIR/lifecycle.py" ]; then
    echo "[${COMPONENT}] ERROR: hook config lifecycle not found: ${SCRIPT_DIR}/lifecycle.py" >&2
    exit 1
fi

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    for home in "${TRAE_HOMES[@]}"; do
        echo "DRY-RUN: merge tokenless hook groups into ${home}/hooks.json"
    done
    exit 0
fi

for home in "${TRAE_HOMES[@]}"; do
    python3 -B "$SCRIPT_DIR/lifecycle.py" merge \
        --trae-home "$home" \
        --template "$HOOKS_TEMPLATE" \
        --dispatcher "$DISPATCHER"
    echo "[${COMPONENT}] ${AGENT} hooks installed into ${home}/hooks.json."
done

echo "[${COMPONENT}] Restart Trae to load the hooks."
