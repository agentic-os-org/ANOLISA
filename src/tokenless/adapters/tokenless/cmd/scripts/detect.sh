#!/usr/bin/env bash
# detect.sh — Check whether Command Code can load the Tokenless mod.
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
    echo "[${COMPONENT}] ${AGENT}: cmd CLI not found" >&2
    exit 2
fi
if [ ! -f "$MOD_SOURCE" ]; then
    echo "[${COMPONENT}] ${AGENT}: mod source missing: ${MOD_SOURCE}" >&2
    exit 2
fi

VERSION="$("$CMD_BIN" --version 2>/dev/null || true)"
if [ -z "$VERSION" ]; then
    echo "[${COMPONENT}] ${AGENT}: cmd CLI did not report a version" >&2
    exit 2
fi

# Command Code resolves ~/.commandcode through HOME/USERPROFILE; there is no
# dedicated config-home variable, so the override is ours.
MODS_DIR="${TOKENLESS_CMD_MODS_DIR:-${COMMANDCODE_HOME:-$HOME/.commandcode}/mods}"
MOD_LINK="$MODS_DIR/tokenless.ts"
if [ -L "$MOD_LINK" ] && [ "$MOD_LINK" -ef "$MOD_SOURCE" ]; then
    echo "[${COMPONENT}] ${AGENT}: ready (${VERSION})"
    exit 0
fi

echo "[${COMPONENT}] ${AGENT}: detected (${VERSION}), mod not installed"
exit 1
