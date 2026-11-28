#!/usr/bin/env bash
# uninstall.sh — Remove only the Tokenless-owned hook entries from Trae's
# global hooks.json.
#
# User-configured hooks are never touched: an entry counts as ours only when its
# whole command matches the shape install.sh generates, and only the events
# install.sh writes are visited. When nothing matches, the user's file is left
# byte-for-byte alone. That logic lives in lifecycle.py, shared with install.sh
# and detect.sh so the three cannot disagree about ownership.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-trae}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
# `dirname`, not ${BASH_SOURCE[0]%/*}: a basename invocation (`bash install.sh`
# from inside this directory) has no slash to strip, so the pattern expansion
# would leave the filename and `cd` into it would fail.
SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"

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
if [ ! -f "$SCRIPT_DIR/lifecycle.py" ]; then
    echo "[${COMPONENT}] ERROR: hook config lifecycle not found: ${SCRIPT_DIR}/lifecycle.py" >&2
    exit 1
fi

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    for home in "${TRAE_HOMES[@]}"; do
        echo "DRY-RUN: remove tokenless hook entries from ${home}/hooks.json"
    done
    exit 0
fi

for home in "${TRAE_HOMES[@]}"; do
    python3 -B "$SCRIPT_DIR/lifecycle.py" strip --trae-home "$home"
done

echo "[${COMPONENT}] ${AGENT} hooks uninstalled."
