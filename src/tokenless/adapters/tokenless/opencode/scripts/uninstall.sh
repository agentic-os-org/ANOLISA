#!/usr/bin/env bash
# uninstall.sh — Remove only the OpenCode plugin link managed by Tokenless.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-opencode}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
PLUGIN_SOURCE="$ADAPTER_DIR/opencode/plugin.js"
CONFIG_HOME="${TOKENLESS_OPENCODE_CONFIG_DIR:-${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}}"
PLUGIN_LINK="$CONFIG_HOME/plugins/tokenless.js"

is_managed_link() {
    [ -L "$PLUGIN_LINK" ] && {
        [ "$PLUGIN_LINK" -ef "$PLUGIN_SOURCE" ] ||
            [ "$(readlink "$PLUGIN_LINK")" = "$PLUGIN_SOURCE" ]
    }
}

if [ ! -e "$PLUGIN_LINK" ] && [ ! -L "$PLUGIN_LINK" ]; then
    echo "[${COMPONENT}] ${AGENT} plugin is already absent."
    exit 0
fi
if ! is_managed_link; then
    echo "[${COMPONENT}] WARNING: leaving unmanaged path untouched: ${PLUGIN_LINK}" >&2
    exit 0
fi

rm "$PLUGIN_LINK"
echo "[${COMPONENT}] ${AGENT} plugin removed."
