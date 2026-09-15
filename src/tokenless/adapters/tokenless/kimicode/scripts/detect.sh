#!/usr/bin/env bash
# detect.sh — Inspect Kimi Code presence and tokenless adapter state.
# Read-only. Tri-state exit aligns with claude-code/openclaw/qwencode detect.sh:
#   0 = installed and ready
#   1 = not installed but installable (prereqs OK)
#   2 = missing prerequisites
set -euo pipefail

COMPONENT="${ANOLISA_COMPONENT:-tokenless}"
AGENT="${ANOLISA_TARGET:-kimicode}"
ADAPTER_DIR="${ANOLISA_ADAPTER_DIR:-$(cd "$(dirname "$0")/../.." && pwd)}"

ADAPTER_SRC="$ADAPTER_DIR/kimicode"

# Shared Kimi data-root resolution: Kimi Code reads ~/.kimi-code (KIMI_CODE_HOME)
# while the wound-down kimi-cli read ~/.kimi (KIMI_SHARE_DIR). See _common.sh.
SCRIPT_DIR="${BASH_SOURCE[0]%/*}"
# shellcheck source=./_common.sh
source "$SCRIPT_DIR/_common.sh"

KIMI_BIN="${KIMI_BIN:-}"
export PATH="$HOME/.local/bin:/usr/local/bin:$PATH"

line()  { printf '[%s] %s\n' "$COMPONENT" "$*"; }
field() { printf '[%s]   %-26s %s\n' "$COMPONENT" "$1" "$2"; }

PREREQ_MISSING=()
INSTALL_MISSING=()
note_prereq_missing()  { PREREQ_MISSING+=("$1"); }
note_install_missing() { INSTALL_MISSING+=("$1"); }

if [ -z "$KIMI_BIN" ]; then
    KIMI_BIN="$(command -v kimi 2>/dev/null || true)"
fi

line "${AGENT} detect"
if [ -n "$KIMI_BIN" ] && [ -x "$KIMI_BIN" ]; then
    KIMI_VER="$("$KIMI_BIN" --version 2>/dev/null | head -n1 | awk '{print $NF}' || echo unknown)"
    field "kimi CLI"            "present (${KIMI_BIN}, v${KIMI_VER})"
else
    field "kimi CLI"            "missing"
    note_prereq_missing "kimi CLI"
fi

# Which data root the installed Kimi CLI reads is a property of the product,
# not of this adapter: _common.sh resolves it (and says why) so this report
# never claims readiness against a directory Kimi does not load.
# Absence is not a prerequisite failure — created on first run.
KIMI_HOME="$(resolve_kimi_home)"
field "kimi data root"        "$KIMI_HOME"
field "kimi data root source" "$(resolve_kimi_home_origin)"
if [ -d "$KIMI_HOME" ]; then
    field "kimi config dir"     "present ($KIMI_HOME)"
else
    field "kimi config dir"     "missing (created on first Kimi run)"
fi

# A second data root on this host means a kimi-cli install was migrated to
# Kimi Code (or both are installed). Only the resolved one is read; report the
# other so leftover hooks there are not mistaken for a working install.
for other_root in "${HOME}/.kimi-code" "${HOME}/.kimi"; do
    if [ "$other_root" != "$KIMI_HOME" ] && [ -d "$other_root" ]; then
        field "other kimi data root" "present ($other_root) — not read by this adapter"
    fi
done

# Check if config.toml exists (hooks are configured here)
if [ -f "$KIMI_HOME/config.toml" ]; then
    field "kimi config.toml"    "present"
    # Check if tokenless hooks are already configured
    if grep -qE 'adapters/tokenless/kimicode/hooks/(run-hook|tool-ready-kimi-wrapper)\.sh|tokenless-tool-ready' "$KIMI_HOME/config.toml" 2>/dev/null; then
        field "tokenless hooks"   "configured"
    else
        field "tokenless hooks"   "not configured"
        note_install_missing "tokenless hooks"
    fi
else
    field "kimi config.toml"    "missing"
    note_install_missing "kimi config.toml"
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

if [ -f "$ADAPTER_SRC/hooks/run-hook.sh" ]; then
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
