#!/bin/bash

set -euo pipefail

# shellcheck source=lib-discover.sh
source "$(dirname "$0")/lib-discover.sh"

HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"
HERMES_BIN="${HERMES_BIN:-}"
HERMES_SKILLS_DIR="${HERMES_SKILLS_DIR:-${HERMES_HOME%/}/skills}"
DRY_RUN="${ANOLISA_DRY_RUN:-0}"
PLUGIN_DST="${HERMES_HOME%/}/plugins/ws-ckpt"
SKILL_DST="${HERMES_SKILLS_DIR%/}/ws-ckpt"

if [ -z "$HERMES_BIN" ]; then
    HERMES_BIN="$(command -v hermes 2>/dev/null || true)"
fi

# 1. Try plugin install (preferred)
if PLUGIN_SRC=$(find_plugin_src hermes); then
    if [ "$DRY_RUN" = "1" ]; then
        echo "DRY-RUN: mkdir -p $(dirname "$PLUGIN_DST")"
        echo "DRY-RUN: ln -sfn $PLUGIN_SRC $PLUGIN_DST"
        if [ -n "$HERMES_BIN" ] && [ -x "$HERMES_BIN" ]; then
            echo "DRY-RUN: HERMES_HOME=${HERMES_HOME%/} timeout --kill-after=5s 30s $HERMES_BIN plugins enable ws-ckpt"
        else
            echo "DRY-RUN: hermes CLI not found; plugin would need manual enable"
        fi
        exit 0
    fi
    mkdir -p "$(dirname "$PLUGIN_DST")"
    ln -sfn "$PLUGIN_SRC" "$PLUGIN_DST"
    if [ -n "$HERMES_BIN" ] && [ -x "$HERMES_BIN" ]; then
        # Gateway reload can stall; bound even a CLI that ignores SIGTERM.
        HERMES_HOME="${HERMES_HOME%/}" timeout --kill-after=5s 30s "$HERMES_BIN" plugins enable ws-ckpt || {
            echo "Warning: hermes plugins enable failed or timed out; enable ws-ckpt manually." >&2
        }
    else
        echo "hermes CLI not found; add 'ws-ckpt' to plugins.enabled in ${HERMES_HOME%/}/config.yaml."
    fi
    echo "hermes ws-ckpt plugin linked: $PLUGIN_DST -> $PLUGIN_SRC"
    exit 0
fi

# 2. Fallback to skill install
if SKILL_SRC=$(find_skill_src); then
    if [ "$DRY_RUN" = "1" ]; then
        echo "DRY-RUN: mkdir -p $SKILL_DST"
        echo "DRY-RUN: cp -pr $SKILL_SRC/. $SKILL_DST/"
        exit 0
    fi
    mkdir -p "$SKILL_DST"
    cp -pr "$SKILL_SRC"/. "$SKILL_DST/"
    echo "skill installed to $SKILL_DST (from $SKILL_SRC)"
else
    print_search_error
    exit 1
fi
