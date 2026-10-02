#!/usr/bin/env bash
# detect.sh — Check whether OpenCode can load the Tokenless local plugin.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-opencode}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
PLUGIN_SOURCE="$ADAPTER_DIR/opencode/plugin.js"

OPENCODE_BIN="${OPENCODE_BIN:-}"
if [ -z "$OPENCODE_BIN" ]; then
    OPENCODE_BIN="$(command -v opencode 2>/dev/null || true)"
fi

if [ -z "$OPENCODE_BIN" ] || { [ ! -x "$OPENCODE_BIN" ] && ! command -v "$OPENCODE_BIN" >/dev/null 2>&1; }; then
    echo "[${COMPONENT}] ${AGENT}: opencode CLI not found" >&2
    exit 2
fi
if [ ! -f "$PLUGIN_SOURCE" ]; then
    echo "[${COMPONENT}] ${AGENT}: plugin source missing: ${PLUGIN_SOURCE}" >&2
    exit 2
fi

VERSION="$($OPENCODE_BIN --version 2>/dev/null || true)"
if [ -z "$VERSION" ]; then
    echo "[${COMPONENT}] ${AGENT}: opencode CLI did not report a version" >&2
    exit 2
fi

CONFIG_HOME="${TOKENLESS_OPENCODE_CONFIG_DIR:-${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}}"
PLUGIN_LINK="$CONFIG_HOME/plugins/tokenless.js"
if [ -L "$PLUGIN_LINK" ] && [ "$PLUGIN_LINK" -ef "$PLUGIN_SOURCE" ]; then
    echo "[${COMPONENT}] ${AGENT}: ready (${VERSION})"
    exit 0
fi

echo "[${COMPONENT}] ${AGENT}: detected (${VERSION}), plugin not installed"
exit 1
