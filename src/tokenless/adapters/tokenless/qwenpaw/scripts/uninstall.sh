#!/usr/bin/env bash
# uninstall.sh — Remove the tokenless plugin from QwenPaw.
set -euo pipefail

# A minimal runner may omit HOME. Under `set -u` an unguarded expansion aborts
# this uninstaller with exit 1 -- the same status it otherwise reports for "not installed but installable",
# so the crash would reach the caller as a verdict instead of as an error.
# Resolve HOME once and derive every user-scope path from it; an empty value
# leaves the destination unresolved and the script refuses to act on a path
# under /.

HOME_DIR="${HOME:-}"

AGENT="${ANOLISA_TARGET:-qwenpaw}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
# Resolve the working directory like QwenPaw: QWENPAW_WORKING_DIR, else
# COPAW_WORKING_DIR, else a legacy ~/.copaw, else ~/.qwenpaw.
PLUGIN_ID="tokenless"
QWENPAW_WORKING_DIR="${QWENPAW_WORKING_DIR:-${COPAW_WORKING_DIR:-}}"
if [ -z "$QWENPAW_WORKING_DIR" ]; then
    if [ -z "$HOME_DIR" ]; then
        # Without a home there is no directory to derive the working
        # directory from, and guessing one would put the plugin under /.
        QWENPAW_WORKING_DIR=""
        other=""
    elif [ -d "$HOME_DIR/.copaw" ]; then
        QWENPAW_WORKING_DIR="$HOME_DIR/.copaw"; other="$HOME_DIR/.qwenpaw"
    else
        QWENPAW_WORKING_DIR="$HOME_DIR/.qwenpaw"; other="$HOME_DIR/.copaw"
    fi
    # ~/.copaw may have appeared or vanished since the plugin was installed:
    # prefer the default location that actually holds it.
    if [ -n "$other" ] && [ ! -d "$QWENPAW_WORKING_DIR/plugins/$PLUGIN_ID" ] \
            && [ -d "$other/plugins/$PLUGIN_ID" ]; then
        QWENPAW_WORKING_DIR="$other"
    fi
fi
QWENPAW_BIN="${QWENPAW_BIN:-}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"
USER_LOCAL_BIN="${HOME_DIR:+$HOME_DIR/.local/bin:}"
export PATH="${QWENPAW_HOME:-${HOME_DIR:+$HOME_DIR/.qwenpaw}}/bin:${USER_LOCAL_BIN}/usr/local/bin:$PATH"

PLUGIN_DST="${QWENPAW_WORKING_DIR%/}/plugins/${PLUGIN_ID}"

# The uninstaller removes the plugin directory from the QwenPaw working
# directory, so that directory has to be known before anything is removed. A
# path built from a missing HOME would sit under /, and an explicit
# QWENPAW_WORKING_DIR or COPAW_WORKING_DIR still names a real directory.
if [ -z "$QWENPAW_WORKING_DIR" ]; then
    echo "[${COMPONENT}] ERROR: cannot resolve the QwenPaw working directory: HOME is unset and no override names it." >&2
    echo "[${COMPONENT}] Set HOME, QWENPAW_WORKING_DIR or COPAW_WORKING_DIR to the directory to use, then retry." >&2
    exit 1
fi

echo "[${COMPONENT}] Uninstalling ${AGENT} plugin..."

if [ -z "$QWENPAW_BIN" ]; then
    QWENPAW_BIN="$(command -v qwenpaw 2>/dev/null || true)"
fi

if [ "$DRY_RUN" = "1" ]; then
    if [ -n "$QWENPAW_BIN" ] && [ -x "$QWENPAW_BIN" ]; then
        echo "DRY-RUN: QWENPAW_WORKING_DIR=${QWENPAW_WORKING_DIR%/} $QWENPAW_BIN plugin uninstall ${PLUGIN_ID}"
    else
        echo "DRY-RUN: qwenpaw CLI not found; skip CLI uninstall"
    fi
    echo "DRY-RUN: rm -rf $PLUGIN_DST"
    exit 0
fi

if [ ! -d "$PLUGIN_DST" ]; then
    echo "[${COMPONENT}] no ${PLUGIN_ID} plugin is installed in $PLUGIN_DST; set QWENPAW_WORKING_DIR if QwenPaw uses another working directory."
    exit 0
fi

# The CLI hot-unloads a running QwenPaw, prompts for confirmation, removes
# the plugin directory, and exits 0 even when it fails: the directory still
# being there means the plugin may still be loaded in a running QwenPaw.
if [ -n "$QWENPAW_BIN" ] && [ -x "$QWENPAW_BIN" ]; then
    printf 'y\n' | QWENPAW_WORKING_DIR="${QWENPAW_WORKING_DIR%/}" "$QWENPAW_BIN" plugin uninstall "$PLUGIN_ID" || true
    if [ -f "$PLUGIN_DST/plugin.json" ]; then
        echo "[${COMPONENT}] qwenpaw plugin uninstall did not remove $PLUGIN_DST; a running QwenPaw may still have the plugin loaded. Restart QwenPaw and run this script again." >&2
        exit 1
    fi
else
    echo "[${COMPONENT}] qwenpaw CLI not found; removing $PLUGIN_DST without hot-unloading. Restart QwenPaw if it is running." >&2
fi

rm -rf "$PLUGIN_DST"

echo "[${COMPONENT}] ${AGENT} plugin uninstalled."
