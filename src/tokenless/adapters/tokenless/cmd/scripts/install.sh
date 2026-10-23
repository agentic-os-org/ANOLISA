#!/usr/bin/env bash
# install.sh — Register Tokenless in Command Code's user-scope mods directory.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-cmd}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
MOD_SOURCE="$ADAPTER_DIR/cmd/tokenless.ts"

CMD_BIN="${CMD_BIN:-}"
if [ -z "$CMD_BIN" ]; then
    CMD_BIN="$(command -v cmd 2>/dev/null || true)"
fi
if [ -z "$CMD_BIN" ] || { [ ! -x "$CMD_BIN" ] && ! command -v "$CMD_BIN" >/dev/null 2>&1; }; then
    echo "[${COMPONENT}] cmd CLI not found — skipping ${AGENT} mod installation."
    exit 0
fi
if [ ! -f "$MOD_SOURCE" ]; then
    echo "[${COMPONENT}] ERROR: Command Code mod source not found: ${MOD_SOURCE}" >&2
    exit 1
fi

MODS_DIR="${TOKENLESS_CMD_MODS_DIR:-${COMMANDCODE_HOME:-$HOME/.commandcode}/mods}"
MOD_LINK="$MODS_DIR/tokenless.ts"

# Command Code discovers user mods in ~/.commandcode/mods plus the directories
# its own "mods.paths" setting lists; it reads neither TOKENLESS_CMD_MODS_DIR
# nor COMMANDCODE_HOME. Both variables only move this installer's link, so a
# link placed anywhere else is created but never loaded. Report that instead of
# a bare success the host cannot honour.
HOST_MODS_DIR="$HOME/.commandcode/mods"

warn_when_not_host_scanned() {
    if [ "$MODS_DIR" = "$HOST_MODS_DIR" ]; then
        return 0
    fi
    echo "[${COMPONENT}] WARNING: Command Code scans ${HOST_MODS_DIR}, not ${MODS_DIR}." >&2
    echo "[${COMPONENT}] WARNING: TOKENLESS_CMD_MODS_DIR and COMMANDCODE_HOME only move this link; the ${AGENT} mod stays inactive until ${MODS_DIR} is listed under \"mods.paths\" in $HOME/.commandcode/settings.json, or is loaded per session with: cmd --mod ${MOD_LINK}" >&2
}

is_managed_link() {
    [ -L "$MOD_LINK" ] && {
        [ "$MOD_LINK" -ef "$MOD_SOURCE" ] ||
            [ "$(readlink "$MOD_LINK")" = "$MOD_SOURCE" ]
    }
}

if [ -e "$MOD_LINK" ] || [ -L "$MOD_LINK" ]; then
    if is_managed_link; then
        echo "[${COMPONENT}] ${AGENT} mod already installed at ${MOD_LINK}."
        warn_when_not_host_scanned
        exit 0
    fi
    echo "[${COMPONENT}] ERROR: refusing to replace unmanaged path: ${MOD_LINK}" >&2
    exit 1
fi

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    echo "DRY-RUN: ln -s ${MOD_SOURCE} ${MOD_LINK}"
    exit 0
fi

mkdir -p "$MODS_DIR"
ln -s "$MOD_SOURCE" "$MOD_LINK"
if ! is_managed_link; then
    echo "[${COMPONENT}] ERROR: failed to verify Command Code mod link: ${MOD_LINK}" >&2
    exit 1
fi

echo "[${COMPONENT}] ${AGENT} mod installed at ${MOD_LINK}."
warn_when_not_host_scanned
echo "[${COMPONENT}] Start a new Command Code session (or run /reload) to load the mod."
