#!/usr/bin/env bash
# uninstall.sh — Remove only the Command Code mod link managed by Tokenless.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-cmd}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
MOD_SOURCE="$ADAPTER_DIR/cmd/tokenless.ts"
MODS_DIR="${TOKENLESS_CMD_MODS_DIR:-${COMMANDCODE_HOME:-$HOME/.commandcode}/mods}"
MOD_LINK="$MODS_DIR/tokenless.ts"

is_managed_link() {
    [ -L "$MOD_LINK" ] && {
        [ "$MOD_LINK" -ef "$MOD_SOURCE" ] ||
            [ "$(readlink "$MOD_LINK")" = "$MOD_SOURCE" ]
    }
}

if [ ! -e "$MOD_LINK" ] && [ ! -L "$MOD_LINK" ]; then
    echo "[${COMPONENT}] ${AGENT} mod is already absent."
    exit 0
fi
if ! is_managed_link; then
    echo "[${COMPONENT}] WARNING: leaving unmanaged path untouched: ${MOD_LINK}" >&2
    exit 0
fi

# Preview before the removal, mirroring the installer's
# ANOLISA_DRY_RUN handling: a dry run must leave the live link in place.
if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    echo "DRY-RUN: rm ${MOD_LINK}"
    exit 0
fi

rm "$MOD_LINK"
echo "[${COMPONENT}] ${AGENT} mod removed."
