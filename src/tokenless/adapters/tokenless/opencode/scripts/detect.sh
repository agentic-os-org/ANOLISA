#!/usr/bin/env bash
# detect.sh — Check whether OpenCode can load the Tokenless local plugin.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-opencode}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
PLUGIN_SOURCE="$ADAPTER_DIR/opencode/plugin.js"

# OpenCode 1.0.0-1.3.3 call every module export as a plugin function, so the
# object default export OpenCode 2 requires aborts their plugin initialization
# with `TypeError: fn is not a function`. 1.3.4 is the first release that reads
# an object default export, so it is the floor the manifest advertises.
MIN_OPENCODE_VERSION="1.3.4"

# Prints the first dotted-number version in the CLI's --version output.
opencode_version_number() {
    printf '%s' "$1" | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -n 1 || true
}

# opencode_version_at_least <have> <want>. A non-numeric suffix ("3-test") is
# ignored and a missing component counts as 0.
opencode_version_at_least() {
    local have want index=0 part_have part_want
    IFS='.' read -r -a have <<< "$1"
    IFS='.' read -r -a want <<< "$2"
    while [ "$index" -lt "${#want[@]}" ]; do
        part_have="${have[$index]:-0}"
        part_want="${want[$index]:-0}"
        part_have="${part_have//[!0-9]/}"
        part_want="${part_want//[!0-9]/}"
        if [ "${part_have:-0}" -gt "${part_want:-0}" ]; then return 0; fi
        if [ "${part_have:-0}" -lt "${part_want:-0}" ]; then return 1; fi
        index=$((index + 1))
    done
    return 0
}

# opencode_version_supported <raw --version output>. An unparseable version is
# not gated: refusing to install on a host we cannot read would disable the
# adapter for every future release whose --version wording changes.
opencode_version_supported() {
    local number
    number="$(opencode_version_number "$1")"
    if [ -z "$number" ]; then
        return 0
    fi
    opencode_version_at_least "$number" "$MIN_OPENCODE_VERSION"
}

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

if ! opencode_version_supported "$VERSION"; then
    echo "[${COMPONENT}] ${AGENT}: ${VERSION} is older than the supported OpenCode ${MIN_OPENCODE_VERSION}" >&2
    echo "[${COMPONENT}] ${AGENT}: such releases call every plugin export as a function" >&2
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
