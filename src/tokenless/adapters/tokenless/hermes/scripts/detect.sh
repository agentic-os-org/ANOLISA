#!/usr/bin/env bash
# detect.sh — Inspect tokenless Hermes integration. Read-only.
#
# Reports hermes CLI, Hermes home, tokenless plugin install state, runtime
# binary, and adapter resource availability. Exit codes:
#   0 = installed and ready
#   1 = not installed but installable
#   2 = missing prerequisites
set -euo pipefail

AGENT="${ANOLISA_TARGET:-hermes}"
COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"
HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"
HERMES_BIN="${HERMES_BIN:-}"
export PATH="$HOME/.local/bin:${HERMES_HOME%/}/bin:/usr/local/bin:$PATH"

PLUGIN_ID="tokenless"
PLUGIN_SRC="$ADAPTER_DIR/hermes"

line()  { printf '[%s] %s\n' "$COMPONENT" "$*"; }
field() { printf '[%s]   %-26s %s\n' "$COMPONENT" "$1" "$2"; }

PREREQ_MISSING=()
INSTALL_MISSING=()
note_prereq_missing() { PREREQ_MISSING+=("$1"); }
note_install_missing() { INSTALL_MISSING+=("$1"); }

if [ -z "$HERMES_BIN" ]; then
    HERMES_BIN="$(command -v hermes 2>/dev/null || true)"
fi

line "${AGENT} detect"
if [ -n "$HERMES_BIN" ] && [ -x "$HERMES_BIN" ]; then
    field "hermes CLI" "present (${HERMES_BIN})"
else
    field "hermes CLI" "missing"
    note_prereq_missing "hermes CLI"
fi

if [ -d "$HERMES_HOME" ]; then
    field "hermes home" "present (${HERMES_HOME})"
else
    field "hermes home" "not installed (${HERMES_HOME})"
    note_install_missing "hermes home"
fi

runtime_bin="$(command -v tokenless 2>/dev/null || true)"
if [ -n "$runtime_bin" ]; then
    field "tokenless binary" "present (${runtime_bin})"
else
    field "tokenless binary" "missing"
    note_prereq_missing "tokenless binary"
fi

if [ -d "$PLUGIN_SRC" ] && [ -f "$PLUGIN_SRC/plugin.yaml" ] && [ -f "$PLUGIN_SRC/__init__.py" ]; then
    field "plugin resource" "present (${PLUGIN_SRC})"
else
    field "plugin resource" "missing (${PLUGIN_SRC})"
    note_prereq_missing "plugin resource"
fi

plugin_dst="${HERMES_HOME%/}/plugins/${PLUGIN_ID}"
if [ -d "$plugin_dst" ] || [ -L "$plugin_dst" ]; then
    field "${PLUGIN_ID} plugin" "installed (${plugin_dst})"
else
    field "${PLUGIN_ID} plugin" "missing (${plugin_dst})"
    note_install_missing "${PLUGIN_ID} plugin"
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
