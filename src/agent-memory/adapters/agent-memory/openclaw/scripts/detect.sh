#!/usr/bin/env bash
# detect.sh — Check if OpenClaw is installed and compatible.
# Exit 0 = ready to install, non-0 = not available.
set -euo pipefail

AGENT="${ANOLISA_TARGET:-openclaw}"
COMPONENT="${ANOLISA_COMPONENT:-agent-memory}"
OPENCLAW_HOME="${OPENCLAW_HOME:-$HOME/.openclaw}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-$OPENCLAW_HOME}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"
OPENCLAW_HOME="${OPENCLAW_HOME%/}"

if [ -d "$OPENCLAW_STATE_DIR" ]; then
    echo "[${COMPONENT}] ${AGENT}: detected ${OPENCLAW_STATE_DIR} config directory"
    exit 0
fi

if command -v openclaw &>/dev/null; then
    echo "[${COMPONENT}] ${AGENT}: detected openclaw binary"
    exit 0
fi

echo "[${COMPONENT}] ${AGENT}: not detected (neither ${OPENCLAW_STATE_DIR} nor openclaw binary found)" >&2
exit 1