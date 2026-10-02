#!/usr/bin/env bash
# detect.sh — Inspect tokenless OpenClaw integration. Read-only.
#
# Reports OpenClaw CLI, tokenless plugin install state, runtime
# artifact (dist/index.js), and adapter resource. Exits 0 when the OpenClaw
# CLI and the tokenless plugin are both present; non-zero otherwise.
set -euo pipefail

COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
AGENT="${ANOLISA_TARGET:-openclaw}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
OPENCLAW_HOME="${OPENCLAW_HOME:-$HOME/.openclaw}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR:-$OPENCLAW_HOME}"
OPENCLAW_STATE_DIR="${OPENCLAW_STATE_DIR%/}"
OPENCLAW_HOME="${OPENCLAW_HOME%/}"
OPENCLAW_BIN="${OPENCLAW_BIN:-}"
export PATH="$HOME/.local/bin:${OPENCLAW_STATE_DIR%/}/bin:/usr/local/bin:$PATH"

PLUGIN_ID="tokenless"
PLUGIN_SRC="$ADAPTER_DIR/openclaw"

line()  { printf '[%s] %s\n' "$COMPONENT" "$*"; }
field() { printf '[%s]   %-26s %s\n' "$COMPONENT" "$1" "$2"; }

PREREQ_MISSING=()
INSTALL_MISSING=()
note_prereq_missing() { PREREQ_MISSING+=("$1"); }
note_install_missing() { INSTALL_MISSING+=("$1"); }

if [ -z "$OPENCLAW_BIN" ]; then
    OPENCLAW_BIN="$(command -v openclaw 2>/dev/null || true)"
fi

line "${AGENT} detect"
if [ -n "$OPENCLAW_BIN" ] && [ -x "$OPENCLAW_BIN" ]; then
    field "openclaw CLI" "present (${OPENCLAW_BIN})"
else
    field "openclaw CLI" "missing"
    note_prereq_missing "openclaw CLI"
fi

if [ -d "$OPENCLAW_STATE_DIR" ]; then
    field "openclaw home" "present (${OPENCLAW_STATE_DIR})"
else
    field "openclaw home" "not installed (${OPENCLAW_STATE_DIR})"
    note_install_missing "openclaw home"
fi

plugin_state="missing"
plugin_detail="$PLUGIN_ID"
if [ -n "$OPENCLAW_BIN" ] && [ -x "$OPENCLAW_BIN" ]; then
    plugins_json="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" plugins list --json 2>/dev/null || true)"
    plugins_txt="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$OPENCLAW_STATE_DIR" "$OPENCLAW_BIN" plugins list 2>/dev/null || true)"
    if grep -qE "\"id\"[[:space:]]*:[[:space:]]*\"${PLUGIN_ID}\"" <<<"$plugins_json" \
       || grep -qE "(^|[[:space:]])${PLUGIN_ID}([[:space:]]|$)" <<<"$plugins_txt"; then
        plugin_state="listed"
        plugin_detail="$PLUGIN_ID (openclaw plugins list)"
    fi
fi
if [ "$plugin_state" = "missing" ] && [ -d "${OPENCLAW_STATE_DIR%/}/extensions/${PLUGIN_ID}" ]; then
    plugin_state="installed"
    plugin_detail="${OPENCLAW_STATE_DIR%/}/extensions/${PLUGIN_ID}"
fi
if [ "$plugin_state" != "missing" ]; then
    field "${PLUGIN_ID} plugin" "${plugin_state} (${plugin_detail})"
else
    field "${PLUGIN_ID} plugin" "missing"
    note_install_missing "${PLUGIN_ID} plugin"
fi

runtime_bin="$(command -v tokenless 2>/dev/null || true)"
if [ -n "$runtime_bin" ]; then
    field "tokenless binary" "present (${runtime_bin})"
else
    field "tokenless binary" "missing"
    note_prereq_missing "tokenless binary"
fi

if [ -d "$PLUGIN_SRC" ]; then
    field "adapter resource" "present (${PLUGIN_SRC})"
else
    field "adapter resource" "missing (${PLUGIN_SRC})"
    note_prereq_missing "adapter resource"
fi

if [ -f "$PLUGIN_SRC/dist/index.js" ]; then
    field "plugin build artifact" "present"
else
    field "plugin build artifact" "missing (${PLUGIN_SRC}/dist/index.js)"
    note_prereq_missing "plugin build artifact"
fi

if [ ${#PREREQ_MISSING[@]} -gt 0 ]; then
    line "${AGENT}: missing prerequisites (${PREREQ_MISSING[*]})"
    exit 2
fi
if [ ${#INSTALL_MISSING[@]} -gt 0 ]; then
    line "${AGENT}: not installed (ready to install)"
    exit 1
fi
line "${AGENT}: ready"
exit 0
