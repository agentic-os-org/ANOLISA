#!/usr/bin/env bash
# install.sh — Register Tokenless in OpenCode's global local-plugin directory.
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
    echo "[${COMPONENT}] opencode CLI not found — skipping ${AGENT} plugin installation."
    exit 0
fi
if [ ! -f "$PLUGIN_SOURCE" ]; then
    echo "[${COMPONENT}] ERROR: OpenCode plugin source not found: ${PLUGIN_SOURCE}" >&2
    exit 1
fi

CONFIG_HOME="${TOKENLESS_OPENCODE_CONFIG_DIR:-${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}}"
PLUGIN_DIR="$CONFIG_HOME/plugins"
PLUGIN_LINK="$PLUGIN_DIR/tokenless.js"

is_managed_link() {
    [ -L "$PLUGIN_LINK" ] && {
        [ "$PLUGIN_LINK" -ef "$PLUGIN_SOURCE" ] ||
            [ "$(readlink "$PLUGIN_LINK")" = "$PLUGIN_SOURCE" ]
    }
}

if [ -e "$PLUGIN_LINK" ] || [ -L "$PLUGIN_LINK" ]; then
    if is_managed_link; then
        echo "[${COMPONENT}] ${AGENT} plugin already installed at ${PLUGIN_LINK}."
        exit 0
    fi
    echo "[${COMPONENT}] ERROR: refusing to replace unmanaged path: ${PLUGIN_LINK}" >&2
    exit 1
fi

if [ "${ANOLISA_DRY_RUN:-0}" = "1" ]; then
    echo "DRY-RUN: ln -s ${PLUGIN_SOURCE} ${PLUGIN_LINK}"
    exit 0
fi

mkdir -p "$PLUGIN_DIR"
ln -s "$PLUGIN_SOURCE" "$PLUGIN_LINK"
if ! is_managed_link; then
    echo "[${COMPONENT}] ERROR: failed to verify OpenCode plugin link: ${PLUGIN_LINK}" >&2
    exit 1
fi

echo "[${COMPONENT}] ${AGENT} plugin installed at ${PLUGIN_LINK}."
echo "[${COMPONENT}] Restart OpenCode to load the plugin."
