#!/usr/bin/env bash
# install.sh — Install tokenless plugin into Hermes Agent via symlink + enable.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-hermes}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"

# A minimal runner may omit HOME. Under `set -u` an unguarded expansion aborts
# this installer with exit 1 before it can say why, and unlike the detector this
# script writes to the filesystem. Resolve HOME once and derive every user-scope
# path from it, so an explicit HERMES_HOME override keeps working and a missing
# home is reported before any mkdir or symlink is created.
HOME_DIR="${HOME:-}"
# A relative HOME would resolve every path below against whatever directory
# the caller happened to be in. A home has to be an absolute path; anything
# else is treated as no home at all, which is the case the guard below
# already reports.
case "$HOME_DIR" in
    /*) ;;
    [A-Za-z]:[\\/]*) ;;
    *) HOME_DIR="" ;;
esac
USER_LOCAL_BIN="${HOME_DIR:+$HOME_DIR/.local/bin:}"
HERMES_HOME="${HERMES_HOME:-${HOME_DIR:+$HOME_DIR/.hermes}}"
HERMES_BIN="${HERMES_BIN:-}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"
export PATH="${USER_LOCAL_BIN}${HERMES_HOME:+${HERMES_HOME%/}/bin:}/usr/local/bin:$PATH"

PLUGIN_SRC="$ADAPTER_DIR/hermes"
PLUGIN_DST="${HERMES_HOME:+${HERMES_HOME%/}/plugins/tokenless}"

echo "[${COMPONENT}] Installing ${AGENT} plugin..."

if [ -z "$HERMES_HOME" ]; then
    echo "[${COMPONENT}] Cannot resolve the Hermes home: HOME is unset and HERMES_HOME is not set." >&2
    echo "[${COMPONENT}] Set HOME or HERMES_HOME to the Hermes installation directory, then retry." >&2
    exit 1
fi

if [ ! -d "$PLUGIN_SRC" ]; then
    echo "[${COMPONENT}] Plugin source not found: $PLUGIN_SRC" >&2
    exit 1
fi

if [ -z "$HERMES_BIN" ]; then
    HERMES_BIN="$(command -v hermes 2>/dev/null || true)"
fi

if [ ! -f "$PLUGIN_SRC/plugin.yaml" ] || [ ! -f "$PLUGIN_SRC/__init__.py" ]; then
    echo "[${COMPONENT}] Missing plugin.yaml or __init__.py in $PLUGIN_SRC" >&2
    exit 1
fi

if [ "$DRY_RUN" = "1" ]; then
    echo "DRY-RUN: mkdir -p $PLUGIN_DST"
    echo "DRY-RUN: ln -sfn $PLUGIN_SRC/__init__.py $PLUGIN_DST/__init__.py"
    echo "DRY-RUN: ln -sfn $PLUGIN_SRC/plugin.yaml $PLUGIN_DST/plugin.yaml"
    if [ -n "$HERMES_BIN" ] && [ -x "$HERMES_BIN" ]; then
        echo "DRY-RUN: HERMES_HOME=${HERMES_HOME%/} $HERMES_BIN plugins enable tokenless"
    else
        echo "DRY-RUN: hermes CLI not found; plugin would need manual enable"
    fi
    exit 0
fi

mkdir -p "$PLUGIN_DST"

# Use symlinks so plugin stays synced with system install
ln -sfn "$PLUGIN_SRC/__init__.py" "$PLUGIN_DST/__init__.py"
ln -sfn "$PLUGIN_SRC/plugin.yaml" "$PLUGIN_DST/plugin.yaml"

echo "[${COMPONENT}] ${AGENT} plugin linked to $PLUGIN_DST (from $PLUGIN_SRC)."

# Enable via hermes CLI if available (adds to plugins.enabled in config.yaml)
if [ -n "$HERMES_BIN" ] && [ -x "$HERMES_BIN" ]; then
    echo "[${COMPONENT}] Enabling ${AGENT} plugin..."
    HERMES_HOME="${HERMES_HOME%/}" "$HERMES_BIN" plugins enable tokenless || {
        echo "[${COMPONENT}] Warning: hermes plugins enable failed — enable manually via config.yaml."
    }
else
    echo "[${COMPONENT}] hermes CLI not found — add 'tokenless' to plugins.enabled in ${HERMES_HOME%/}/config.yaml."
fi
