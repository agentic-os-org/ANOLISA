#!/usr/bin/env bash
# detect-hermes.sh — Inspect ws-ckpt Hermes integration. Read-only.
#
# Reports hermes CLI, Hermes home, ws-ckpt plugin link/skill fallback, the
# ws-ckpt runtime binary, and adapter plugin/skill sources. Exit codes:
#   0 = installed and ready
#   1 = not installed but installable
#   2 = missing prerequisites
set -euo pipefail

# shellcheck source=lib-discover.sh
source "$(dirname "$0")/lib-discover.sh"

COMPONENT="${ANOLISA_COMPONENT:-ws-ckpt}"
AGENT="${ANOLISA_TARGET:-hermes}"
HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"
HERMES_BIN="${HERMES_BIN:-}"
HERMES_SKILLS_DIR="${HERMES_SKILLS_DIR:-${HERMES_HOME%/}/skills}"
export PATH="$HOME/.local/bin:${HERMES_HOME%/}/bin:/usr/local/bin:$PATH"

PLUGIN_ID="ws-ckpt"

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

# Plugin link under HERMES_HOME/plugins/ws-ckpt (preferred install path).
plugin_dst="${HERMES_HOME%/}/plugins/${PLUGIN_ID}"
if [ -L "$plugin_dst" ] || [ -d "$plugin_dst" ]; then
    field "${PLUGIN_ID} plugin" "installed (${plugin_dst})"
    PLUGIN_INSTALLED=1
else
    field "${PLUGIN_ID} plugin" "missing (${plugin_dst})"
    PLUGIN_INSTALLED=0
fi

# Skill fallback under HERMES_SKILLS_DIR.
skill_dst="${HERMES_SKILLS_DIR%/}/${PLUGIN_ID}"
if [ -f "$skill_dst/SKILL.md" ]; then
    field "skill fallback" "present (${skill_dst})"
    SKILL_INSTALLED=1
else
    field "skill fallback" "missing (${skill_dst})"
    SKILL_INSTALLED=0
fi

if [ "$PLUGIN_INSTALLED" = "0" ] && [ "$SKILL_INSTALLED" = "0" ]; then
    note_install_missing "${PLUGIN_ID} plugin or skill"
fi

# Runtime binary — ws-ckpt CLI used by the plugin's snapshot operations.
runtime_bin="$(command -v ws-ckpt 2>/dev/null || true)"
if [ -n "$runtime_bin" ]; then
    field "ws-ckpt binary" "present (${runtime_bin})"
else
    field "ws-ckpt binary" "missing"
    note_prereq_missing "ws-ckpt binary"
fi

# Adapter source resources — plugin and skill source for (re-)install.
plugin_src="$(find_plugin_src hermes 2>/dev/null || true)"
field "plugin resource" "${plugin_src:--}"
skill_src="$(find_skill_src 2>/dev/null || true)"
field "skill resource" "${skill_src:--}"
if [ -z "$plugin_src" ] && [ -z "$skill_src" ]; then
    note_prereq_missing "plugin or skill resource"
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
