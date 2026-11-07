#!/usr/bin/env bash
# detect.sh — Check whether OpenCode can load the Tokenless local plugin.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-opencode}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
PLUGIN_SOURCE="$ADAPTER_DIR/opencode/plugin.js"

# A minimal runner may omit HOME. Under `set -u` an unguarded expansion aborts
# this detector with exit 1 -- the same status it otherwise reports for "not
# installed but installable", so the crash would reach the caller as a verdict
# instead of as an error. Resolve HOME once and derive every user-scope path
# from it; an empty value skips the probe below rather than guessing a path
# under /.
HOME_DIR="${HOME:-}"

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

# The config root comes from an explicit override when one is set, and
# otherwise from the user's config directory. The resolution below is the same
# expression install.sh and uninstall.sh use, so this detector always looks
# where they write. Without a HOME there is no such directory: the fallback
# path would be synthetic, naming a location that does not exist on this host.
# Say so on stderr instead of presenting it as an ordinary directory the
# operator could go and inspect, and leave the verdict on stdout unchanged.
if [ -n "$HOME_DIR" ]; then
    CONFIG_HOME="${TOKENLESS_OPENCODE_CONFIG_DIR:-${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME_DIR/.config}/opencode}}"
elif [ -n "${TOKENLESS_OPENCODE_CONFIG_DIR:-}" ] || [ -n "${OPENCODE_CONFIG_DIR:-}" ] || [ -n "${XDG_CONFIG_HOME:-}" ]; then
    CONFIG_HOME="${TOKENLESS_OPENCODE_CONFIG_DIR:-${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-}/opencode}}"
else
    CONFIG_HOME=""
    echo "[${COMPONENT}] ${AGENT}: HOME is unset and no config override is set, so there is no OpenCode config directory to inspect; the config path is synthetic and was not probed" >&2
    echo "[${COMPONENT}] ${AGENT}: set HOME, XDG_CONFIG_HOME, OPENCODE_CONFIG_DIR or TOKENLESS_OPENCODE_CONFIG_DIR to point at the real one" >&2
fi

if [ -n "$CONFIG_HOME" ]; then
    PLUGIN_LINK="$CONFIG_HOME/plugins/tokenless.js"
    if [ -L "$PLUGIN_LINK" ] && [ "$PLUGIN_LINK" -ef "$PLUGIN_SOURCE" ]; then
        echo "[${COMPONENT}] ${AGENT}: ready (${VERSION})"
        exit 0
    fi
fi

echo "[${COMPONENT}] ${AGENT}: detected (${VERSION}), plugin not installed"
exit 1
