#!/usr/bin/env bash
# detect.sh — Inspect Trae presence and the tokenless hook state.
# Read-only. Tri-state exit aligns with claude-code/qwencode detect.sh:
#   0 = installed and ready
#   1 = not installed but installable (prereqs OK)
#   2 = missing prerequisites
set -euo pipefail

COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
AGENT="${ANOLISA_TARGET:-trae}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"

ADAPTER_SRC="$ADAPTER_DIR/trae"
MARKER="TOKENLESS_AGENT_ID=trae"

TRAE_BIN="${TRAE_BIN:-}"
export PATH="$HOME/.local/bin:/usr/local/bin:$PATH"

line()  { printf '[%s] %s\n' "$COMPONENT" "$*"; }
field() { printf '[%s]   %-26s %s\n' "$COMPONENT" "$1" "$2"; }

PREREQ_MISSING=()
INSTALL_MISSING=()
note_prereq_missing()  { PREREQ_MISSING+=("$1"); }
note_install_missing() { INSTALL_MISSING+=("$1"); }

if [ -z "$TRAE_BIN" ]; then
    TRAE_BIN="$(command -v trae 2>/dev/null || true)"
fi

line "${AGENT} detect"
if [ -n "$TRAE_BIN" ] && [ -x "$TRAE_BIN" ]; then
    field "trae CLI"            "present (${TRAE_BIN})"
else
    # Trae is an IDE; many installations expose no CLI. Informational only —
    # presence is decided by the edition home directories below.
    field "trae CLI"            "missing (informational)"
fi

# Trae editions: CN (~/.trae-cn) and international (~/.trae). Both are
# created on first run; absence of both means Trae is not installed.
TRAE_HOMES=()
for home in "$HOME/.trae-cn" "$HOME/.trae"; do
    if [ -d "$home" ]; then
        TRAE_HOMES+=("$home")
        field "trae home"         "present ($home)"
    fi
done
if [ ${#TRAE_HOMES[@]} -eq 0 ]; then
    field "trae home"           "missing (~/.trae-cn / ~/.trae)"
    note_prereq_missing "Trae (no ~/.trae-cn or ~/.trae)"
fi

# Report tokenless hook registration per detected edition.
for home in ${TRAE_HOMES[@]+"${TRAE_HOMES[@]}"}; do
    hooks_file="$home/hooks.json"
    if [ -f "$hooks_file" ] && grep -qF "$MARKER" "$hooks_file" 2>/dev/null; then
        field "hooks ($home)"     "installed"
    else
        field "hooks ($home)"     "not installed"
        note_install_missing "hooks for $home"
    fi
done

if [ -f "$ADAPTER_SRC/hooks/hooks.json" ]; then
    field "hooks template"      "present"
else
    field "hooks template"      "missing (trae/hooks/hooks.json)"
    note_prereq_missing "hooks template"
fi

if [ -f "$ADAPTER_SRC/hooks/run-hook.sh" ]; then
    field "hook dispatcher"     "present"
else
    field "hook dispatcher"     "missing (trae/hooks/run-hook.sh)"
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
