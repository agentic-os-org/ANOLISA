#!/usr/bin/env bash
# install.sh — Install tokenless plugin into Hermes Agent via symlink + enable.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-hermes}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"
HERMES_BIN="${HERMES_BIN:-}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"
export PATH="$HOME/.local/bin:${HERMES_HOME%/}/bin:/usr/local/bin:$PATH"

PLUGIN_SRC="$ADAPTER_DIR/hermes"
PLUGIN_DST="${HERMES_HOME%/}/plugins/tokenless"

echo "[${COMPONENT}] Installing ${AGENT} plugin..."

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
