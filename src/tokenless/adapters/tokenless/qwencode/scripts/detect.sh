#!/usr/bin/env bash
# detect.sh — Inspect Qwen Code presence and the tokenless plugin state.
# Read-only. Tri-state exit aligns with claude-code/openclaw detect.sh:
#   0 = installed and ready
#   1 = not installed but installable (prereqs OK)
#   2 = missing prerequisites
set -euo pipefail

COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
AGENT="${ANOLISA_TARGET:-qwencode}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"

PLUGIN_ID="tokenless"
PLUGIN_SRC="$ADAPTER_DIR/qwencode"

QWEN_BIN="${QWEN_BIN:-}"
export PATH="$HOME/.local/bin:/usr/local/bin:$PATH"

line()  { printf '[%s] %s\n' "$COMPONENT" "$*"; }
field() { printf '[%s]   %-26s %s\n' "$COMPONENT" "$1" "$2"; }

PREREQ_MISSING=()
INSTALL_MISSING=()
note_prereq_missing()  { PREREQ_MISSING+=("$1"); }
note_install_missing() { INSTALL_MISSING+=("$1"); }

if [ -z "$QWEN_BIN" ]; then
    QWEN_BIN="$(command -v qwen 2>/dev/null || true)"
fi

line "${AGENT} detect"
if [ -n "$QWEN_BIN" ] && [ -x "$QWEN_BIN" ]; then
    QWEN_VER="$("$QWEN_BIN" --version 2>/dev/null | awk '{print $1}' || echo unknown)"
    field "qwen CLI"            "present (${QWEN_BIN}, v${QWEN_VER})"
else
    field "qwen CLI"            "missing"
    note_prereq_missing "qwen CLI"
fi

# Informational only: Qwen Code stores global settings, extensions, and
# session data under ~/.qwen/ (upstream default).  Absence is not a
# prerequisite failure — the directory is created on first run.
if [ -d "$HOME/.qwen" ]; then
    field "qwen config dir"     "present ($HOME/.qwen)"
else
    field "qwen config dir"     "missing (created on first Qwen Code run)"
fi

if [ -f "$PLUGIN_SRC/qwen-extension.json" ]; then
    field "qwen-extension.json"   "present"
else
    field "qwen-extension.json"   "missing"
    note_prereq_missing "qwen-extension.json"
fi

if [ -n "$QWEN_BIN" ] && [ -x "$QWEN_BIN" ]; then
    if "$QWEN_BIN" extensions list 2>/dev/null | grep -qE "(^|[[:space:]])${PLUGIN_ID}([[:space:]]|$)"; then
        field "plugin install"      "installed (${PLUGIN_ID})"
    else
        field "plugin install"      "not installed"
        note_install_missing "${PLUGIN_ID}"
    fi
fi

if command -v python3 &>/dev/null; then
    field "python3"               "present ($(command -v python3))"
else
    field "python3"               "missing"
    note_prereq_missing "python3"
fi

if command -v jq &>/dev/null; then
    field "jq"                    "present ($(command -v jq))"
else
    field "jq"                    "missing (tool-ready hook disabled)"
fi

runtime_bin="$(command -v tokenless 2>/dev/null || true)"
if [ -n "$runtime_bin" ]; then
    field "tokenless binary"      "present (${runtime_bin})"
else
    field "tokenless binary"      "missing"
    note_prereq_missing "tokenless binary"
fi

rtk_bin="$(command -v rtk 2>/dev/null || true)"
if [ -n "$rtk_bin" ]; then
    field "rtk binary"            "present (${rtk_bin})"
else
    field "rtk binary"            "missing"
    note_prereq_missing "rtk binary"
fi

# Shared hook scripts live under FHS; warn when missing so user knows to run
# `make install` (or install the RPM) before adapter actually fires.
SHARED_HOOKS_DIR=""
for d in /usr/local/share/anolisa/adapters/tokenless/common/hooks \
         /usr/share/anolisa/adapters/tokenless/common/hooks \
         "$HOME/.local/share/anolisa/adapters/tokenless/common/hooks"; do
    if [ -d "$d" ]; then SHARED_HOOKS_DIR="$d"; break; fi
done
if [ -n "$SHARED_HOOKS_DIR" ]; then
    field "shared hooks dir"      "present ($SHARED_HOOKS_DIR)"
else
    field "shared hooks dir"      "missing (run: make -C src/tokenless install)"
    note_prereq_missing "shared hooks dir"
fi

if [ -f "$PLUGIN_SRC/hooks/run-hook.sh" ]; then
    field "hook dispatcher"       "present"
else
    field "hook dispatcher"       "missing (hooks/run-hook.sh)"
    note_prereq_missing "hook dispatcher"
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
