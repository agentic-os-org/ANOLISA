#!/usr/bin/env bash
# detect.sh — Inspect WorkBuddy / CodeBuddy presence and the tokenless hook state.
# Read-only. Tri-state exit aligns with claude-code/qwencode detect.sh:
#   0 = installed and ready
#   1 = not installed but installable (prereqs OK)
#   2 = missing prerequisites
set -euo pipefail

COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
AGENT="${ANOLISA_TARGET:-workbuddy}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"

ADAPTER_SRC="$ADAPTER_DIR/workbuddy"
MARKER="TOKENLESS_AGENT_ID=workbuddy"
CODEBUDDY_HOME="${CODEBUDDY_HOME:-$HOME/.codebuddy}"

CODEBUDDY_BIN="${CODEBUDDY_BIN:-}"
export PATH="$HOME/.local/bin:/usr/local/bin:$PATH"

line()  { printf '[%s] %s\n' "$COMPONENT" "$*"; }
field() { printf '[%s]   %-26s %s\n' "$COMPONENT" "$1" "$2"; }

PREREQ_MISSING=()
INSTALL_MISSING=()
note_prereq_missing()  { PREREQ_MISSING+=("$1"); }
note_install_missing() { INSTALL_MISSING+=("$1"); }

if [ -z "$CODEBUDDY_BIN" ]; then
    CODEBUDDY_BIN="$(command -v codebuddy 2>/dev/null || true)"
fi

line "${AGENT} detect"
if [ -n "$CODEBUDDY_BIN" ] && [ -x "$CODEBUDDY_BIN" ]; then
    field "codebuddy CLI"       "present (${CODEBUDDY_BIN})"
else
    # WorkBuddy desktop installations may expose no CLI. Informational only —
    # presence is decided by the .codebuddy home directory below.
    field "codebuddy CLI"       "missing (informational)"
fi

# WorkBuddy desktop, CodeBuddy Code CLI and WorkBuddy Enterprise share the
# ~/.codebuddy protocol family; the directory is created on first run.
if [ -d "$CODEBUDDY_HOME" ]; then
    field "codebuddy home"      "present (${CODEBUDDY_HOME})"
else
    field "codebuddy home"      "missing (${CODEBUDDY_HOME})"
    note_prereq_missing "WorkBuddy/CodeBuddy (no ${CODEBUDDY_HOME})"
fi

settings_file="$CODEBUDDY_HOME/settings.json"
if [ -f "$settings_file" ] && grep -qF "$MARKER" "$settings_file" 2>/dev/null; then
    field "hooks"               "installed (${settings_file})"
else
    field "hooks"               "not installed"
    note_install_missing "hooks for ${settings_file}"
fi

if [ -f "$ADAPTER_SRC/hooks/hooks.json" ]; then
    field "hooks template"      "present"
else
    field "hooks template"      "missing (workbuddy/hooks/hooks.json)"
    note_prereq_missing "hooks template"
fi

if [ -f "$ADAPTER_SRC/hooks/run-hook.sh" ]; then
    field "hook dispatcher"     "present"
else
    field "hook dispatcher"     "missing (workbuddy/hooks/run-hook.sh)"
    note_prereq_missing "hook dispatcher"
fi

if command -v python3 &>/dev/null; then
    field "python3"             "present ($(command -v python3))"
else
    field "python3"             "missing"
    note_prereq_missing "python3"
fi

# jq is required by tool_ready_hook.sh; absence disables that hook only
# (rewrite + compress-response still work). Treat as informational.
if command -v jq &>/dev/null; then
    field "jq"                  "present ($(command -v jq))"
else
    field "jq"                  "missing (tool-ready hook disabled)"
fi

runtime_bin="$(command -v tokenless 2>/dev/null || true)"
if [ -n "$runtime_bin" ]; then
    field "tokenless binary"    "present (${runtime_bin})"
else
    field "tokenless binary"    "missing"
    note_prereq_missing "tokenless binary"
fi

rtk_bin="$(command -v rtk 2>/dev/null || true)"
if [ -n "$rtk_bin" ]; then
    field "rtk binary"          "present (${rtk_bin})"
else
    field "rtk binary"          "missing"
    note_prereq_missing "rtk binary"
fi

# Shared hook scripts live under FHS; warn when missing so user knows to run
# `make install` (or the RPM) before the adapter actually fires.
SHARED_HOOKS_DIR=""
for d in /usr/local/share/anolisa/adapters/tokenless/common/hooks \
         /usr/share/anolisa/adapters/tokenless/common/hooks \
         "$HOME/.local/share/anolisa/adapters/tokenless/common/hooks"; do
    if [ -d "$d" ]; then SHARED_HOOKS_DIR="$d"; break; fi
done
if [ -n "$SHARED_HOOKS_DIR" ]; then
    field "shared hooks dir"    "present ($SHARED_HOOKS_DIR)"
else
    field "shared hooks dir"    "missing (run: make -C src/tokenless install)"
    note_prereq_missing "shared hooks dir"
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
